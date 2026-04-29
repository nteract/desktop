#!/usr/bin/env bash
set -euo pipefail

# publish-apt.sh — publishes a .deb to a Cloudflare R2-backed APT repository
# Usage: publish-apt.sh --channel <nightly|stable> [--keep-last <N>] <path-to-deb>

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
CHANNEL=""
KEEP_LAST=20
DEB_PATH=""

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel)
      [[ $# -ge 2 ]] || { echo "ERROR: --channel requires an argument" >&2; exit 1; }
      CHANNEL="$2"
      shift 2
      ;;
    --keep-last)
      [[ $# -ge 2 ]] || { echo "ERROR: --keep-last requires an argument" >&2; exit 1; }
      KEEP_LAST="$2"
      shift 2
      ;;
    --help|-h)
      echo "Usage: publish-apt.sh --channel <nightly|stable> [--keep-last <N>] <path-to-deb>"
      exit 0
      ;;
    -*)
      echo "ERROR: Unknown option: $1" >&2
      exit 1
      ;;
    *)
      if [[ -z "$DEB_PATH" ]]; then
        DEB_PATH="$1"
      else
        echo "ERROR: Unexpected positional argument: $1" >&2
        exit 1
      fi
      shift
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Validate arguments
# ---------------------------------------------------------------------------
if [[ -z "$CHANNEL" ]]; then
  echo "ERROR: --channel is required (nightly or stable)" >&2
  exit 1
fi

if [[ "$CHANNEL" != "nightly" && "$CHANNEL" != "stable" ]]; then
  echo "ERROR: --channel must be 'nightly' or 'stable', got: '$CHANNEL'" >&2
  exit 1
fi

if [[ -z "$DEB_PATH" ]]; then
  echo "ERROR: positional argument <path-to-deb> is required" >&2
  exit 1
fi

if [[ ! -f "$DEB_PATH" || ! -r "$DEB_PATH" ]]; then
  echo "ERROR: .deb file not found or not readable: $DEB_PATH" >&2
  exit 1
fi

if ! [[ "$KEEP_LAST" =~ ^[1-9][0-9]*$ ]]; then
  echo "ERROR: --keep-last must be a positive integer, got: '$KEEP_LAST'" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Validate required environment variables
# ---------------------------------------------------------------------------
: "${CF_ACCOUNT_ID:?ERROR: CF_ACCOUNT_ID is required}"
: "${AWS_ACCESS_KEY_ID:?ERROR: AWS_ACCESS_KEY_ID is required}"
: "${AWS_SECRET_ACCESS_KEY:?ERROR: AWS_SECRET_ACCESS_KEY is required}"
: "${AWS_DEFAULT_REGION:?ERROR: AWS_DEFAULT_REGION is required}"
: "${R2_BUCKET_NAME:?ERROR: R2_BUCKET_NAME is required}"
: "${R2_PUBLIC_URL:?ERROR: R2_PUBLIC_URL is required}"
: "${GPG_KEY_ID:?ERROR: GPG_KEY_ID is required}"
: "${GPG_PRIVATE_KEY:?ERROR: GPG_PRIVATE_KEY is required}"

R2_ENDPOINT="https://${CF_ACCOUNT_ID}.r2.cloudflarestorage.com"

echo "==> Channel:    $CHANNEL"
echo "==> Keep last:  $KEEP_LAST"
echo "==> .deb path:  $DEB_PATH"
echo "==> R2 bucket:  $R2_BUCKET_NAME"
echo "==> R2 endpoint: $R2_ENDPOINT"

# ---------------------------------------------------------------------------
# Step 2: Import GPG key
# ---------------------------------------------------------------------------
echo ""
echo "==> Importing GPG private key..."
echo "$GPG_PRIVATE_KEY" | base64 -d | gpg --batch --import
echo "    Import complete. Verifying key is available..."

# Confirm the key we expect is actually in the keyring
if ! gpg --list-secret-keys "$GPG_KEY_ID" > /dev/null 2>&1; then
  echo "ERROR: GPG key '$GPG_KEY_ID' not found in keyring after import" >&2
  exit 1
fi
echo "    Secret key verified: $GPG_KEY_ID"

# Export the public key to confirm it's usable
PUBLIC_KEY=$(gpg --armor --export "$GPG_KEY_ID")
if [[ -z "$PUBLIC_KEY" ]]; then
  echo "ERROR: Failed to export public key for '$GPG_KEY_ID'" >&2
  exit 1
fi
echo "    Public key export OK ($(echo "$PUBLIC_KEY" | wc -l) lines)"

# ---------------------------------------------------------------------------
# Step 3: Extract version from the .deb
# ---------------------------------------------------------------------------
echo ""
echo "==> Extracting version from .deb..."
VERSION=$(dpkg-deb --field "$DEB_PATH" Version)
if [[ -z "$VERSION" ]]; then
  echo "ERROR: Could not extract Version field from $DEB_PATH" >&2
  exit 1
fi
echo "    Version: $VERSION"

INSTALL_PACKAGE="nteract"
APT_LIST_NAME="nteract"
if [[ "$CHANNEL" == "nightly" ]]; then
  INSTALL_PACKAGE="nteract-nightly"
  APT_LIST_NAME="nteract-nightly"
fi

# ---------------------------------------------------------------------------
# Step 4: Derive the versioned pool filename
# ---------------------------------------------------------------------------
echo ""
echo "==> Deriving pool filename..."
POOL_FILENAME="nteract-${CHANNEL}_${VERSION}_amd64.deb"
POOL_KEY="pool/main/n/nteract/${POOL_FILENAME}"
echo "    Pool filename: $POOL_FILENAME"
echo "    Pool key:      $POOL_KEY"

# ---------------------------------------------------------------------------
# Step 5: Set up temp working directory
# ---------------------------------------------------------------------------
echo ""
echo "==> Setting up temp working directory..."
WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT

POOL_DIR="$WORK_DIR/pool/main/n/nteract"
BINARY_DIR="$WORK_DIR/dists/$CHANNEL/main/binary-amd64"
DIST_DIR="$WORK_DIR/dists/$CHANNEL"

mkdir -p "$POOL_DIR" "$BINARY_DIR"
echo "    Work dir: $WORK_DIR"

# ---------------------------------------------------------------------------
# Step 6: Copy .deb into local pool under versioned name
# ---------------------------------------------------------------------------
echo ""
echo "==> Copying .deb into local pool..."
cp "$DEB_PATH" "$POOL_DIR/$POOL_FILENAME"
echo "    Copied to: $POOL_DIR/$POOL_FILENAME"

# ---------------------------------------------------------------------------
# Step 7: Download existing Packages file from R2
# ---------------------------------------------------------------------------
echo ""
echo "==> Fetching existing Packages file from R2..."
PACKAGES_KEY="dists/$CHANNEL/main/binary-amd64/Packages"
if aws s3 cp "s3://$R2_BUCKET_NAME/$PACKAGES_KEY" "$BINARY_DIR/Packages.existing" \
     --endpoint-url "$R2_ENDPOINT" 2>/dev/null; then
  EXISTING_COUNT=$(grep -c "^Package:" "$BINARY_DIR/Packages.existing" || true)
  echo "    Downloaded existing Packages ($EXISTING_COUNT existing entries)"
else
  touch "$BINARY_DIR/Packages.existing"
  echo "    No existing Packages file found (first publish)"
fi

# ---------------------------------------------------------------------------
# Step 8: Generate Packages entry for the new .deb
# ---------------------------------------------------------------------------
echo ""
echo "==> Scanning new .deb to generate Packages entry..."
(cd "$WORK_DIR" && dpkg-scanpackages --arch amd64 pool/ > "$BINARY_DIR/Packages.new")
echo "    Generated entry:"
sed 's/^/      /' "$BINARY_DIR/Packages.new"

# ---------------------------------------------------------------------------
# Step 9: Merge: prepend new entry to existing Packages
# ---------------------------------------------------------------------------
echo ""
echo "==> Merging new entry with existing Packages..."
cat "$BINARY_DIR/Packages.new" "$BINARY_DIR/Packages.existing" \
  > "$BINARY_DIR/Packages.merged"
MERGED_COUNT=$(grep -c "^Package:" "$BINARY_DIR/Packages.merged" || true)
echo "    Merged Packages has $MERGED_COUNT entries"

# ---------------------------------------------------------------------------
# Step 10: Retention — nightly only
# ---------------------------------------------------------------------------
if [[ "$CHANNEL" == "nightly" ]]; then
  echo ""
  echo "==> Pruning nightly entries (keeping last $KEEP_LAST)..."
  python3 /usr/local/bin/prune-packages.py \
    --packages   "$BINARY_DIR/Packages.merged" \
    --keep-last  "$KEEP_LAST" \
    --output     "$BINARY_DIR/Packages" \
    --delete-list "$WORK_DIR/to_delete.txt"
else
  echo ""
  echo "==> Stable channel — skipping retention, using full merged Packages..."
  cp "$BINARY_DIR/Packages.merged" "$BINARY_DIR/Packages"
  touch "$WORK_DIR/to_delete.txt"
fi

# ---------------------------------------------------------------------------
# Step 12: Compress Packages
# ---------------------------------------------------------------------------
echo ""
echo "==> Compressing Packages..."
gzip  --keep --best "$BINARY_DIR/Packages"
xz    --keep --best "$BINARY_DIR/Packages"
echo "    Created Packages.gz and Packages.xz"

# ---------------------------------------------------------------------------
# Step 13: Generate Release file
# ---------------------------------------------------------------------------
echo ""
echo "==> Generating Release file..."

MD5_LINES=""
SHA1_LINES=""
SHA256_LINES=""

for variant in Packages Packages.gz Packages.xz; do
  FILE="$BINARY_DIR/$variant"
  SIZE=$(stat -c %s "$FILE")
  MD5=$(md5sum    "$FILE" | awk '{print $1}')
  SHA1=$(sha1sum  "$FILE" | awk '{print $1}')
  SHA256=$(sha256sum "$FILE" | awk '{print $1}')
  MD5_LINES="${MD5_LINES} ${MD5}  ${SIZE}  main/binary-amd64/${variant}\n"
  SHA1_LINES="${SHA1_LINES} ${SHA1}  ${SIZE}  main/binary-amd64/${variant}\n"
  SHA256_LINES="${SHA256_LINES} ${SHA256}  ${SIZE}  main/binary-amd64/${variant}\n"
done

DATE=$(date -u -R)
RELEASE="$DIST_DIR/Release"

{
  echo "Origin: nteract"
  echo "Label: nteract"
  echo "Suite: ${CHANNEL}"
  echo "Codename: ${CHANNEL}"
  echo "Architectures: amd64"
  echo "Components: main"
  echo "Description: nteract desktop application (${CHANNEL} builds)"
  echo "Date: ${DATE}"
  echo "MD5Sum:"
  printf "%b" "$MD5_LINES"
  echo "SHA1:"
  printf "%b" "$SHA1_LINES"
  echo "SHA256:"
  printf "%b" "$SHA256_LINES"
} > "$RELEASE"
echo "    Release file written"

# ---------------------------------------------------------------------------
# Step 14: Sign Release
# ---------------------------------------------------------------------------
echo ""
echo "==> Signing Release..."

gpg --default-key "$GPG_KEY_ID" \
    --batch --yes --clearsign \
    --output "$DIST_DIR/InRelease" \
    "$DIST_DIR/Release"
echo "    InRelease written"

gpg --default-key "$GPG_KEY_ID" \
    --batch --yes --detach-sign --armor \
    --output "$DIST_DIR/Release.gpg" \
    "$DIST_DIR/Release"
echo "    Release.gpg written"

# ---------------------------------------------------------------------------
# Step 15: Upload new .deb to R2 pool
# ---------------------------------------------------------------------------
echo ""
echo "==> Uploading .deb to R2 pool..."
aws s3 cp "$POOL_DIR/$POOL_FILENAME" \
  "s3://$R2_BUCKET_NAME/$POOL_KEY" \
  --endpoint-url "$R2_ENDPOINT" \
  --content-type "application/vnd.debian.binary-package"
echo "    Uploaded: $POOL_KEY"

# ---------------------------------------------------------------------------
# Step 16: Upload updated index files
# ---------------------------------------------------------------------------
echo ""
echo "==> Uploading index files to R2..."
rm -f "$BINARY_DIR/Packages.existing" "$BINARY_DIR/Packages.new" "$BINARY_DIR/Packages.merged"
aws s3 sync "$DIST_DIR" \
  "s3://$R2_BUCKET_NAME/dists/$CHANNEL/" \
  --endpoint-url "$R2_ENDPOINT" \
  --cache-control "no-cache, no-store, must-revalidate" \
  --delete
echo "    Index files uploaded"

# ---------------------------------------------------------------------------
# Step 17: Delete pruned old .debs from R2 (nightly only)
# ---------------------------------------------------------------------------
if [[ "$CHANNEL" == "nightly" && -s "$WORK_DIR/to_delete.txt" ]]; then
  echo ""
  echo "==> Deleting pruned old versions from R2..."
  while IFS= read -r old_key; do
    echo "    Deleting: $old_key"
    aws s3 rm "s3://$R2_BUCKET_NAME/$old_key" \
      --endpoint-url "$R2_ENDPOINT" || echo "    WARNING: failed to delete $old_key (non-fatal)"
  done < "$WORK_DIR/to_delete.txt"
fi

# ---------------------------------------------------------------------------
# Step 18: Upload public key (first run only)
# ---------------------------------------------------------------------------
echo ""
echo "==> Checking for public keyring in R2..."
if ! aws s3 ls "s3://$R2_BUCKET_NAME/nteract-keyring.gpg" \
     --endpoint-url "$R2_ENDPOINT" > /dev/null 2>&1; then
  echo "    Not found — uploading public key..."
  gpg --armor --export "$GPG_KEY_ID" \
    | aws s3 cp - "s3://$R2_BUCKET_NAME/nteract-keyring.gpg" \
        --endpoint-url "$R2_ENDPOINT" \
        --content-type "application/pgp-keys"
  echo "    Uploaded nteract-keyring.gpg"
else
  echo "    Public key already present — skipping"
fi

# ---------------------------------------------------------------------------
# Step 19: Print user setup instructions
# ---------------------------------------------------------------------------
echo ""
echo "Done! Published ${INSTALL_PACKAGE} ${VERSION}"
echo ""
echo "Users can install with:"
echo ""
echo "  curl -fsSL ${R2_PUBLIC_URL}/nteract-keyring.gpg \\"
echo "    | sudo gpg --dearmor --yes -o /usr/share/keyrings/nteract-keyring.gpg"
echo ""
echo "  echo \"deb [arch=amd64 signed-by=/usr/share/keyrings/nteract-keyring.gpg] \\"
echo "    ${R2_PUBLIC_URL} ${CHANNEL} main\" \\"
echo "    | sudo tee /etc/apt/sources.list.d/${APT_LIST_NAME}.list"
echo ""
echo "  sudo apt update && sudo apt install ${INSTALL_PACKAGE}"
