# APT Repository Publisher

> **Status:** disabled. Linux desktop releases currently publish AppImage only.
> DEB/RPM/APT publication is intentionally unsupported until there is a
> first-class distro-native lifecycle for the user-local `runtimed` daemon. See
> `contributing/releasing.md`.

Publishes nteract `.deb` packages to the Cloudflare R2-backed APT repository at
`https://apt.runtimed.com`. Supports `nightly` and `stable` channels in the
same bucket using standard Debian repository layout.

## Files

| File | Purpose |
|------|---------|
| `Dockerfile` | Publisher image — builds the APT index and uploads to R2 |
| `Dockerfile.test` | Test image — configured to install from the live R2 repo |
| `publish-apt.sh` | Main publish script, runs inside the publisher container |
| `prune-packages.py` | Python helper that prunes old nightly versions from the Packages index |
| `docker-compose.yml` | Defines `publisher` and `test` services |
| `.env.example` | Template for required credentials |
| `debs/` | Drop `.deb` files here before publishing (gitignored) |

---

## Local setup

### 1. GPG signing key

The publisher signs the APT index with a GPG key. You can use an existing key or generate a dedicated one.

**Option A — use your existing key:**

```bash
# Find your fingerprint
gpg --list-secret-keys --keyid-format long

# Export it (single-line base64, no wrapping)
gpg --armor --export-secret-keys YOUR_FINGERPRINT | base64 -w 0
```

Note: if your key has a passphrase you must strip it first, since the script imports it unattended:

```bash
gpg --edit-key YOUR_FINGERPRINT
# at the gpg> prompt:
passwd
# enter current passphrase, leave new passphrase blank
```

**Option B — generate a dedicated key (no passphrase):**

```bash
gpg --batch --gen-key <<EOF
Key-Type: RSA
Key-Length: 4096
Name-Real: nteract
Name-Email: releases@nteract.io
Expire-Date: 0
%no-protection
EOF

# Get the fingerprint
gpg --list-secret-keys --keyid-format long releases@nteract.io

# Export as base64
gpg --armor --export-secret-keys releases@nteract.io | base64 -w 0
```

### 2. Create `.env`

```bash
cp scripts/apt/.env.example scripts/apt/.env
```

Fill in `scripts/apt/.env`:

```
CF_ACCOUNT_ID=your-cloudflare-account-id
AWS_ACCESS_KEY_ID=your-r2-access-key-id
AWS_SECRET_ACCESS_KEY=your-r2-secret-access-key
AWS_DEFAULT_REGION=auto
R2_BUCKET_NAME=your-bucket-name
R2_PUBLIC_URL=https://apt.runtimed.com
GPG_KEY_ID=YOUR_FINGERPRINT
GPG_PRIVATE_KEY=<output of the base64 export command above>
```

`R2_ENDPOINT` is derived automatically inside the script as `https://${CF_ACCOUNT_ID}.r2.cloudflarestorage.com` — do not set it manually.

### 3. Copy the `.deb` into the debs folder

```bash
cp /path/to/nteract-nightly-linux-x64.deb scripts/apt/debs/
```

---

## Publishing

```bash
docker compose -f scripts/apt/docker-compose.yml run --build --rm publisher \
  --channel nightly /workspace/nteract-nightly-linux-x64.deb
```

For stable:

```bash
docker compose -f scripts/apt/docker-compose.yml run --build --rm publisher \
  --channel stable /workspace/nteract-stable-linux-x64.deb
```

### Retention (nightly only)

Nightly builds keep the 20 most recent versions by default. Override with `--keep-last`:

```bash
# Keep only the last 5 nightly builds
docker compose -f scripts/apt/docker-compose.yml run --rm publisher \
  --channel nightly --keep-last 5 /workspace/nteract-nightly-linux-x64.deb
```

Stable builds are never pruned — all versions are retained.

---

## Testing the repository

The `test` service builds a fresh Ubuntu container with the nteract APT sources pre-configured. It reads `R2_PUBLIC_URL` from your `.env` at build time.

```bash
docker compose -f scripts/apt/docker-compose.yml run --build test
```

Inside the container:

```bash
# Refresh the index
apt update

# Check available versions
apt-cache policy nteract-nightly
apt-cache policy nteract

# Install
apt install nteract-nightly
apt install nteract
```

Note: the app requires a display (GTK) and will crash with a GTK error if you try to run it inside the container — that's expected. A clean `apt install` with no errors is sufficient to confirm the repository is working.

## User install commands

Stable:

```bash
curl -fsSL https://apt.runtimed.com/nteract-keyring.gpg \
  | sudo gpg --dearmor --yes -o /usr/share/keyrings/nteract-keyring.gpg

echo "deb [arch=amd64 signed-by=/usr/share/keyrings/nteract-keyring.gpg] https://apt.runtimed.com stable main" \
  | sudo tee /etc/apt/sources.list.d/nteract.list

sudo apt update
sudo apt install nteract
```

Nightly:

```bash
curl -fsSL https://apt.runtimed.com/nteract-keyring.gpg \
  | sudo gpg --dearmor --yes -o /usr/share/keyrings/nteract-keyring.gpg

echo "deb [arch=amd64 signed-by=/usr/share/keyrings/nteract-keyring.gpg] https://apt.runtimed.com nightly main" \
  | sudo tee /etc/apt/sources.list.d/nteract-nightly.list

sudo apt update
sudo apt install nteract-nightly
```

---

## Repository layout in R2

```
bucket root  (https://apt.runtimed.com)
│
├── nteract-keyring.gpg
│
├── pool/
│   └── main/n/nteract/
│       ├── nteract-nightly_2.0.7-nightly.202603310157_amd64.deb
│       └── nteract-stable_2.1.0_amd64.deb
│
└── dists/
    ├── nightly/
    │   ├── Release
    │   ├── Release.gpg
    │   ├── InRelease
    │   └── main/binary-amd64/
    │       ├── Packages
    │       ├── Packages.gz
    │       └── Packages.xz
    └── stable/
        └── ...
```
