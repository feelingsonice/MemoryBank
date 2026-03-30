#!/usr/bin/env sh
set -eu

REPO_OWNER="feelingsonice"
REPO_NAME="MemoryBank"
RELEASE_BASE_URL="https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/latest/download"

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="${SCRIPT_DIR}"
APP_ROOT="${HOME}/.memory_bank"
BIN_DIR="${APP_ROOT}/bin"
CONFIG_DIR="${APP_ROOT}/config"
INTEGRATIONS_DIR="${APP_ROOT}/integrations"
OPENCODE_INSTALL_DIR="${INTEGRATIONS_DIR}/opencode"
OPENCLAW_INSTALL_DIR="${INTEGRATIONS_DIR}/openclaw"
FROM_SOURCE=0

usage() {
  cat <<'EOF'
Usage: ./install.sh [--from-source] [--help]

Options:
  --from-source  Build the workspace locally and install from this checkout
                 into ~/.memory_bank using the same layout as a GitHub Release.
  --help         Show this help text.
EOF
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --from-source)
        FROM_SOURCE=1
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        echo "Unknown argument: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
    shift
  done
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}" in
    Darwin)
      case "${arch}" in
        x86_64)
          echo "Intel macOS release binaries are not available yet because the current FastEmbed/ONNX Runtime dependency does not publish a supported x86_64-apple-darwin artifact." >&2
          echo "For now, please build from source with ./install.sh --from-source on Intel macOS." >&2
          exit 1
          ;;
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

require_file() {
  path="$1"
  if [ ! -f "${path}" ]; then
    echo "Required file is missing: ${path}" >&2
    exit 1
  fi
}

copy_executable() {
  source_path="$1"
  target_path="$2"
  require_file "${source_path}"
  mkdir -p "$(dirname "${target_path}")"
  cp "${source_path}" "${target_path}"
  chmod 755 "${target_path}"
}

install_source_assets() {
  mkdir -p "${BIN_DIR}" "${CONFIG_DIR}" "${OPENCODE_INSTALL_DIR}" "${OPENCLAW_INSTALL_DIR}"

  copy_executable "${REPO_ROOT}/target/release/mb" "${BIN_DIR}/mb"
  copy_executable "${REPO_ROOT}/target/release/memory-bank-server" "${BIN_DIR}/memory-bank-server"
  copy_executable "${REPO_ROOT}/target/release/memory-bank-hook" "${BIN_DIR}/memory-bank-hook"
  copy_executable "${REPO_ROOT}/target/release/memory-bank-mcp-proxy" "${BIN_DIR}/memory-bank-mcp-proxy"

  require_file "${REPO_ROOT}/config/setup-model-catalog.json"
  cp "${REPO_ROOT}/config/setup-model-catalog.json" "${CONFIG_DIR}/setup-model-catalog.json"

  require_file "${REPO_ROOT}/.opencode/plugins/memory-bank.js"
  cp "${REPO_ROOT}/.opencode/plugins/memory-bank.js" "${OPENCODE_INSTALL_DIR}/memory-bank.js"

  if [ -d "${OPENCLAW_INSTALL_DIR}/memory-bank" ]; then
    rm -rf "${OPENCLAW_INSTALL_DIR}/memory-bank"
  fi
  mkdir -p "${OPENCLAW_INSTALL_DIR}"
  cp -R "${REPO_ROOT}/.openclaw/extensions/memory-bank" "${OPENCLAW_INSTALL_DIR}/memory-bank"
}

install_from_source() {
  require_file "${REPO_ROOT}/Cargo.toml"
  require_file "${REPO_ROOT}/config/setup-model-catalog.json"
  require_file "${REPO_ROOT}/.opencode/plugins/memory-bank.js"
  require_file "${REPO_ROOT}/.openclaw/extensions/memory-bank/index.js"

  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required for --from-source installs." >&2
    exit 1
  fi

  echo "Building Memory Bank from source..."
  cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --workspace --release

  echo "Installing locally built artifacts into ${APP_ROOT}..."
  mkdir -p "${APP_ROOT}"
  install_source_assets
}

install_from_release() {
  TARGET="$(detect_target)"
  ARCHIVE_NAME="memory-bank-${TARGET}.tar.gz"
  CHECKSUM_NAME="SHA256SUMS"

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
}

ensure_path_entry() {
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
}

run_setup() {
  "${BIN_DIR}/mb" setup
}

main() {
  parse_args "$@"

  if [ "${FROM_SOURCE}" -eq 1 ]; then
    install_from_source
  else
    install_from_release
  fi

  ensure_path_entry
  run_setup
}

main "$@"
