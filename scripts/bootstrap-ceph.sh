#!/usr/bin/env bash
# bootstrap-ceph — one command to provision the Ceph RGW user + bucket Dejaveph needs, and
# (optionally) stash the access keys in 1Password so opnix can render them on the gateway.
#
# Run on a host with `radosgw-admin` (a Ceph admin node, or `cephadm shell --`). The bucket is
# created with the AWS CLI against the RGW endpoint.
#
#   scripts/bootstrap-ceph.sh \
#     --endpoint https://rgw.ceph.home.arpa \
#     --bucket dejaveph-xorbs --uid dejaveph \
#     --vault Infrastructure --item dejaveph-ceph-rgw      # last two optional (need `op`)
set -euo pipefail

ENDPOINT="" BUCKET="dejaveph-xorbs" UID_="dejaveph" VAULT="" ITEM="dejaveph-ceph-rgw"
while [ $# -gt 0 ]; do
  case "$1" in
    --endpoint) ENDPOINT="$2"; shift 2 ;;
    --bucket)   BUCKET="$2"; shift 2 ;;
    --uid)      UID_="$2"; shift 2 ;;
    --vault)    VAULT="$2"; shift 2 ;;
    --item)     ITEM="$2"; shift 2 ;;
    -h|--help)  sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$ENDPOINT" ] || { echo "error: --endpoint is required" >&2; exit 2; }
command -v radosgw-admin >/dev/null || { echo "error: radosgw-admin not found — run on a Ceph admin node or via 'cephadm shell --'" >&2; exit 1; }

echo "==> Ensuring RGW user '$UID_'"
if radosgw-admin user info --uid="$UID_" >/dev/null 2>&1; then
  echo "    user exists; reading its keys"
  USER_JSON=$(radosgw-admin user info --uid="$UID_")
else
  echo "    creating user"
  USER_JSON=$(radosgw-admin user create --uid="$UID_" --display-name="Dejaveph Xet CAS" --max-buckets=8)
fi

# Extract the first access/secret key pair (no jq dependency).
ACCESS=$(printf '%s' "$USER_JSON" | grep -o '"access_key": *"[^"]*"' | head -1 | sed 's/.*"access_key": *"\([^"]*\)".*/\1/')
SECRET=$(printf '%s' "$USER_JSON" | grep -o '"secret_key": *"[^"]*"' | head -1 | sed 's/.*"secret_key": *"\([^"]*\)".*/\1/')
[ -n "$ACCESS" ] && [ -n "$SECRET" ] || { echo "error: could not parse access/secret key from radosgw-admin output" >&2; exit 1; }
echo "    access_key_id=$ACCESS"

echo "==> Ensuring bucket '$BUCKET' at $ENDPOINT"
if command -v aws >/dev/null; then
  if AWS_ACCESS_KEY_ID="$ACCESS" AWS_SECRET_ACCESS_KEY="$SECRET" \
       aws --endpoint-url "$ENDPOINT" s3 ls "s3://$BUCKET" >/dev/null 2>&1; then
    echo "    bucket exists"
  else
    AWS_ACCESS_KEY_ID="$ACCESS" AWS_SECRET_ACCESS_KEY="$SECRET" \
      aws --endpoint-url "$ENDPOINT" s3 mb "s3://$BUCKET"
  fi
else
  echo "    (aws CLI not found — create the bucket manually: s3 mb s3://$BUCKET)"
fi

if [ -n "$VAULT" ]; then
  echo "==> Storing keys in 1Password: op://$VAULT/$ITEM"
  if command -v op >/dev/null; then
    if op item get "$ITEM" --vault "$VAULT" >/dev/null 2>&1; then
      op item edit "$ITEM" --vault "$VAULT" "access_key_id=$ACCESS" "secret_access_key=$SECRET" >/dev/null
      echo "    updated existing item"
    else
      op item create --vault "$VAULT" --title "$ITEM" --category 'API Credential' \
        "access_key_id[text]=$ACCESS" "secret_access_key[password]=$SECRET" >/dev/null
      echo "    created item"
    fi
  else
    echo "    (op CLI not found — add the item manually with fields access_key_id, secret_access_key)"
  fi
fi

cat <<EOF

Done. Point the gateway at:
  endpoint = "$ENDPOINT"
  bucket   = "$BUCKET"
  credentialsFile from op://${VAULT:-Infrastructure}/$ITEM (access_key_id, secret_access_key)
See templates/gateway and docs/DEPLOYMENT.md.
EOF
