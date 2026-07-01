#!/usr/bin/env bash
set -euo pipefail

log() {
  printf 'singleton installer: %s\n' "$*" >&2
}

usage() {
  cat >&2 <<'USAGE'
Install the singleton binary from GitHub Releases.

Usage:
  curl -fsSL https://github.com/nrakover/singleton/releases/latest/download/install.sh | bash
  curl -fsSL https://github.com/nrakover/singleton/releases/latest/download/install.sh | bash -s -- --version v0.1.0

Options:
  --version VERSION          Install a specific release tag, such as v0.1.0.
  --install-dir PATH         Install directory. Defaults to $HOME/.local/bin.
  --release-base-url URL     Download assets from a custom release base URL.
  --force                    Reinstall even if the target version is current.
  --dry-run                  Print the resolved download/install plan.
  --help                     Show this help.

Environment:
  SINGLETON_VERSION
  SINGLETON_INSTALL_DIR
  SINGLETON_RELEASE_BASE_URL
  SINGLETON_FORCE_INSTALL=1
USAGE
}

target_triple() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}:${arch}" in
    Darwin:arm64|Darwin:aarch64)
      printf 'aarch64-apple-darwin'
      ;;
    Linux:x86_64|Linux:amd64)
      printf 'x86_64-unknown-linux-gnu'
      ;;
    *)
      log "unsupported platform ${os}/${arch}; install from source or a manual release archive"
      return 1
      ;;
  esac
}

release_base_url() {
  if [[ -n "${release_base_url_override}" ]]; then
    printf '%s' "${release_base_url_override}"
  elif [[ -n "${version}" ]]; then
    printf 'https://github.com/nrakover/singleton/releases/download/%s' "${version}"
  else
    printf 'https://github.com/nrakover/singleton/releases/latest/download'
  fi
}

require_command() {
  local name
  name="$1"
  if ! command -v "${name}" >/dev/null 2>&1; then
    log "required command '${name}' was not found on PATH"
    return 1
  fi
}

verify_checksum() {
  local directory checksum_file
  directory="$1"
  checksum_file="$2"
  if command -v shasum >/dev/null 2>&1; then
    (cd "${directory}" && shasum -a 256 -c "${checksum_file}") >&2
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "${directory}" && sha256sum -c "${checksum_file}") >&2
  else
    log "neither shasum nor sha256sum is available for checksum verification"
    return 1
  fi
}

version_of() {
  local binary
  binary="$1"
  "${binary}" --version 2>/dev/null | awk '{print $NF}'
}

path_contains() {
  local directory
  directory="$1"
  case ":${PATH:-}:" in
    *":${directory}:"*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

version="${SINGLETON_VERSION:-}"
install_dir="${SINGLETON_INSTALL_DIR:-}"
release_base_url_override="${SINGLETON_RELEASE_BASE_URL:-}"
force="${SINGLETON_FORCE_INSTALL:-0}"
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      if [[ $# -lt 2 ]]; then
        log "--version requires a value"
        exit 2
      fi
      version="$2"
      shift 2
      ;;
    --install-dir)
      if [[ $# -lt 2 ]]; then
        log "--install-dir requires a value"
        exit 2
      fi
      install_dir="$2"
      shift 2
      ;;
    --release-base-url)
      if [[ $# -lt 2 ]]; then
        log "--release-base-url requires a value"
        exit 2
      fi
      release_base_url_override="$2"
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      log "unknown option: $1"
      usage
      exit 2
      ;;
  esac
done

if [[ -z "${install_dir}" ]]; then
  if [[ -z "${HOME:-}" ]]; then
    log "HOME is not set; pass --install-dir"
    exit 1
  fi
  install_dir="${HOME}/.local/bin"
fi

target="$(target_triple)"
base_url="$(release_base_url)"
archive="singleton-${target}.tar.gz"
checksum="${archive}.sha256"
binary_target="${install_dir}/singleton"

if [[ "${dry_run}" == "1" ]]; then
  log "install directory: ${install_dir}"
  log "binary target: ${binary_target}"
  log "archive: ${base_url%/}/${archive}"
  log "checksum: ${base_url%/}/${checksum}"
  exit 0
fi

require_command curl
require_command tar

temp_dir="$(mktemp -d)"
trap 'rm -rf "${temp_dir}"' EXIT

log "downloading ${archive} from ${base_url}"
curl -fsSL "${base_url%/}/${archive}" -o "${temp_dir}/${archive}"
curl -fsSL "${base_url%/}/${checksum}" -o "${temp_dir}/${checksum}"
verify_checksum "${temp_dir}" "${checksum}"
tar -xzf "${temp_dir}/${archive}" -C "${temp_dir}"

found_binary="$(find "${temp_dir}" -type f -name singleton -print -quit)"
if [[ -z "${found_binary}" ]]; then
  log "release archive did not contain a singleton binary"
  exit 1
fi

candidate_version="$(version_of "${found_binary}" || true)"
if [[ -z "${candidate_version}" ]]; then
  log "downloaded singleton binary did not report a version; continuing after checksum verification"
fi

existing_version=""
if [[ -x "${binary_target}" ]]; then
  existing_version="$(version_of "${binary_target}" || true)"
fi

if [[ -n "${candidate_version}" && "${existing_version}" == "${candidate_version}" && "${force}" != "1" ]]; then
  log "singleton ${candidate_version} is already installed at ${binary_target}"
else
  mkdir -p "${install_dir}"
  if [[ ! -w "${install_dir}" ]]; then
    log "install directory is not writable: ${install_dir}"
    exit 1
  fi
  install -m 0755 "${found_binary}" "${binary_target}"
  if [[ -n "${existing_version}" && -n "${candidate_version}" ]]; then
    log "updated singleton ${existing_version} -> ${candidate_version} at ${binary_target}"
  elif [[ -n "${candidate_version}" ]]; then
    log "installed singleton ${candidate_version} at ${binary_target}"
  else
    log "installed singleton at ${binary_target}"
  fi
fi

if ! path_contains "${install_dir}"; then
  log "add singleton to PATH, for example: export PATH=\"${install_dir}:\$PATH\""
fi

log "next: singleton install-mcp --client copilot"
