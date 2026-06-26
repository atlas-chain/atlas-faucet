#!/usr/bin/env bash
set -euo pipefail

APP_NAME="${APP_NAME:-atlas-faucet}"
PROFILE="${PROFILE:-release}"
DIST_DIR="${DIST_DIR:-dist}"
TARGET_TRIPLE="${TARGET_TRIPLE:-}"

VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}"
if [[ -z "${VERSION}" ]]; then
  echo "Could not determine package version from Cargo.toml" >&2
  exit 1
fi

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
PACKAGE_TARGET="${TARGET_TRIPLE:-$HOST_TRIPLE}"
GIT_REF="${GITHUB_REF_NAME:-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo local)}"
GIT_SHA="${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}"
PACKAGE_NAME="${APP_NAME}-${VERSION}-${PACKAGE_TARGET}"
STAGING_DIR="${DIST_DIR}/${PACKAGE_NAME}"
ARCHIVE="${DIST_DIR}/${PACKAGE_NAME}.tar.gz"
CHECKSUM="${ARCHIVE}.sha256"

build_args=(build --locked --profile "$PROFILE")
if [[ -n "$TARGET_TRIPLE" ]]; then
  build_args+=(--target "$TARGET_TRIPLE")
fi

cargo "${build_args[@]}"

BINARY_DIR="target"
if [[ -n "$TARGET_TRIPLE" ]]; then
  BINARY_DIR="${BINARY_DIR}/${TARGET_TRIPLE}"
fi
BINARY_DIR="${BINARY_DIR}/${PROFILE}"
BINARY_PATH="${BINARY_DIR}/${APP_NAME}"
if [[ ! -f "$BINARY_PATH" && -f "${BINARY_PATH}.exe" ]]; then
  BINARY_PATH="${BINARY_PATH}.exe"
fi
if [[ ! -f "$BINARY_PATH" ]]; then
  echo "Expected binary at ${BINARY_PATH}" >&2
  exit 1
fi

rm -rf "$STAGING_DIR" "$ARCHIVE" "$CHECKSUM"
mkdir -p "$STAGING_DIR"

install -m 755 "$BINARY_PATH" "$STAGING_DIR/$(basename "$BINARY_PATH")"
cp README.md instructions.md Dockerfile docker-compose.yml "$STAGING_DIR/"

cat > "${STAGING_DIR}/package.json" <<EOF
{
  "name": "${APP_NAME}",
  "version": "${VERSION}",
  "target": "${PACKAGE_TARGET}",
  "profile": "${PROFILE}",
  "gitRef": "${GIT_REF}",
  "gitSha": "${GIT_SHA}",
  "binary": "$(basename "$BINARY_PATH")"
}
EOF

tar -C "$DIST_DIR" -czf "$ARCHIVE" "$PACKAGE_NAME"
(
  cd "$DIST_DIR"
  sha256sum "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")"
)

echo "Created ${ARCHIVE}"
echo "Created ${CHECKSUM}"
