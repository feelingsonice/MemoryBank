#!/usr/bin/env sh
set -eu

REPO_OWNER="feelingsonice"
REPO_NAME="MemoryBank"
RELEASE_BASE_URL="https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/latest/download"

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}" in
    Darwin)
      case "${arch}" in
        x86_64) printf '%s\n' "x86_64-apple-darwin" ;;
        arm64|aarch64) printf '%s\n' "aarch64-apple-darwin" ;;
        *)
          echo "Unsupported macOS architecture: ${arch}" >&2
          exit 1
          ;;
      esac
      ;;
    Linux)
      case "${arch}" in
        x86_64) printf '%s\n' "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) printf '%s\n' "aarch64-unknown-linux-gnu" ;;
        *)
          echo "Unsupported Linux architecture: ${arch}" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      echo "Unsupported operating system: ${os}" >&2
      exit 1
      ;;
  esac
}

verify_checksum() {
  archive_name="$1"
  archive_path="$2"
  checksum_file="$3"

  expected="$(awk -v target="${archive_name}" '$2 == target { print $1 }' "${checksum_file}")"
  if [ -z "${expected}" ]; then
    echo "Missing checksum entry for ${archive_name}" >&2
    exit 1
  fi

  if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${archive_path}" | awk '{ print $1 }')"
  elif command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${archive_path}" | awk '{ print $1 }')"
  else
    echo "Could not find shasum or sha256sum for checksum verification." >&2
    exit 1
  fi

  if [ "${actual}" != "${expected}" ]; then
    echo "Checksum verification failed for ${archive_name}" >&2
    exit 1
  fi
}

TARGET="$(detect_target)"
ARCHIVE_NAME="memory-bank-${TARGET}.tar.gz"
CHECKSUM_NAME="SHA256SUMS"

APP_ROOT="${HOME}/.memory_bank"
TMP_DIR="$(mktemp -d)"
ARCHIVE_PATH="${TMP_DIR}/${ARCHIVE_NAME}"
CHECKSUM_PATH="${TMP_DIR}/${CHECKSUM_NAME}"

cleanup() {
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT INT TERM

mkdir -p "${APP_ROOT}"

curl -fsSL "${RELEASE_BASE_URL}/${ARCHIVE_NAME}" -o "${ARCHIVE_PATH}"
curl -fsSL "${RELEASE_BASE_URL}/${CHECKSUM_NAME}" -o "${CHECKSUM_PATH}"
verify_checksum "${ARCHIVE_NAME}" "${ARCHIVE_PATH}" "${CHECKSUM_PATH}"
tar -xzf "${ARCHIVE_PATH}" -C "${APP_ROOT}"

BIN_DIR="${APP_ROOT}/bin"

case "${SHELL:-}" in
  */zsh)
    SHELL_RC="${HOME}/.zshrc"
    ;;
  */bash)
    SHELL_RC="${HOME}/.bashrc"
    ;;
  *)
    SHELL_RC="${HOME}/.profile"
    ;;
esac

if [ ! -f "${SHELL_RC}" ]; then
  touch "${SHELL_RC}"
fi

if ! grep -q '\.memory_bank/bin' "${SHELL_RC}"; then
  {
    printf '\n# Memory Bank\n'
    printf 'export PATH="$HOME/.memory_bank/bin:$PATH"\n'
  } >> "${SHELL_RC}"
fi

"${BIN_DIR}/mb" setup
