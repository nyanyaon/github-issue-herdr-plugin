#!/bin/sh
#
# Install the viewer binary at ./bin/herdr-issues, which is where the manifest's
# pane entrypoint expects it.
#
# Run by `[[build]]` on `herdr plugin install` only — after the user has
# confirmed the install preview, never on `herdr plugin link`, and with no socket
# or runtime environment. A non-zero exit aborts the install, so this script
# exits non-zero only when it has no binary to leave behind.
#
# It downloads the prebuilt release matching the manifest `version` and the
# detected target, verifies it against `checksums.txt`, and unpacks it. On *any*
# failure — no release for this target, no network, no checksum tool, a checksum
# mismatch, an unknown target — it falls back to `cargo build --release`, so an
# unpublished platform still ends up with a working plugin. An artifact that
# fails verification is deleted unread and never placed or run.
#
# POSIX sh: /bin/sh is dash on most Linux distributions, so nothing bash-only
# here. `pipefail` is enabled only where the shell has it.

set -eu
# shellcheck disable=SC3040 # guarded: the subshell probes for pipefail first.
if (set -o pipefail) 2>/dev/null; then
	set -o pipefail
fi

REPO="nyanyaon/github-issue-herdr-plugin"
BINARY="herdr-issues"
DESTINATION="bin/${BINARY}"

# Test hook, mirroring HERDR_ISSUES_GRAPHQL_URL in the viewer: point the download
# at a local server so the fetch, the verification and the fallback can be
# exercised without a published release. It moves where the artifact *and* its
# checksums come from together, so it weakens no verification — it is the same
# trust boundary as the release itself, and this script already runs as the user.
RELEASE_BASE_URL="${HERDR_ISSUES_RELEASE_BASE_URL:-}"

TEMPORARY_DIRECTORY=""

log() {
	printf '%s\n' "$*" >&2
}

cleanup() {
	if [ -n "${TEMPORARY_DIRECTORY}" ] && [ -d "${TEMPORARY_DIRECTORY}" ]; then
		rm -rf "${TEMPORARY_DIRECTORY}"
	fi
}
trap cleanup EXIT HUP INT TERM

# The plugin root — the directory holding the manifest, and where ./bin must end
# up. Derived from this script's own location rather than trusted from the
# working directory.
plugin_root() {
	CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd
}

# The `version` at the top level of the manifest. Anchored, so `min_herdr_version`
# does not match, and first-wins so a later table cannot shadow it.
manifest_version() {
	sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' herdr-plugin.toml |
		head -n 1
}

# uname to one of the four published targets. Anything else is an unknown target
# and takes the source-build path.
detect_target() {
	operating_system="$(uname -s)"
	machine="$(uname -m)"
	case "${operating_system}:${machine}" in
	Linux:x86_64 | Linux:amd64) printf 'x86_64-unknown-linux-musl\n' ;;
	Linux:aarch64 | Linux:arm64) printf 'aarch64-unknown-linux-musl\n' ;;
	Darwin:x86_64) printf 'x86_64-apple-darwin\n' ;;
	Darwin:arm64 | Darwin:aarch64) printf 'aarch64-apple-darwin\n' ;;
	*) return 1 ;;
	esac
}

# One file, over TLS, failing on any HTTP error status. Redirects are followed
# because GitHub Releases serve assets from a separate download host.
#
# `--proto '=https'` pins the scheme for real installs; it is dropped only when
# the test hook points somewhere that is not https, and verification of the
# downloaded bytes is unchanged either way. Nothing here ever disables TLS
# verification.
download() {
	url="$1"
	output="$2"
	if command -v curl >/dev/null 2>&1; then
		case "${url}" in
		https://*)
			curl --fail --silent --show-error --location \
				--proto '=https' --tlsv1.2 \
				--connect-timeout 10 --max-time 300 \
				--output "${output}" -- "${url}"
			;;
		*)
			curl --fail --silent --show-error --location \
				--connect-timeout 10 --max-time 300 \
				--output "${output}" -- "${url}"
			;;
		esac
	elif command -v wget >/dev/null 2>&1; then
		wget --quiet --timeout=10 --output-document="${output}" -- "${url}"
	else
		log "neither curl nor wget is available"
		return 1
	fi
}

# The SHA-256 of a file, lower-case hex and nothing else. sha256sum on Linux,
# shasum on macOS, openssl as a last resort.
sha256_of() {
	file="$1"
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum -- "${file}" | cut -d ' ' -f 1
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 -- "${file}" | cut -d ' ' -f 1
	elif command -v openssl >/dev/null 2>&1; then
		openssl dgst -sha256 "${file}" | sed 's/.*= *//'
	else
		return 1
	fi
}

# Downloads, verifies and unpacks the prebuilt for ${target} at ${version}.
#
# Every failure returns non-zero with a reason on stderr; the caller turns that
# into a source build. `set -e` is deliberately not relied on here, because this
# function is called as the left operand of `||` — where the shell disables it —
# so each step is checked explicitly.
install_prebuilt() {
	version="$1"
	target="$2"

	archive="${BINARY}-${target}.tar.gz"
	base_url="${RELEASE_BASE_URL}"
	if [ -z "${base_url}" ]; then
		base_url="https://github.com/${REPO}/releases/download/v${version}"
	fi

	TEMPORARY_DIRECTORY="$(mktemp -d)" || {
		log "could not create a temporary directory"
		return 1
	}

	download "${base_url}/${archive}" "${TEMPORARY_DIRECTORY}/${archive}" || {
		log "no prebuilt ${archive} for v${version}"
		return 1
	}
	download "${base_url}/checksums.txt" "${TEMPORARY_DIRECTORY}/checksums.txt" || {
		log "could not download checksums.txt for v${version}"
		return 1
	}

	# Verification happens before the archive is unpacked, so unverified bytes are
	# never written outside the temporary directory and never executed.
	expected="$(sed -n "s/^\([0-9a-fA-F]\{64\}\)[[:space:]][[:space:]]*[*]\{0,1\}${archive}\$/\1/p" \
		"${TEMPORARY_DIRECTORY}/checksums.txt" | head -n 1)" || expected=""
	if [ -z "${expected}" ]; then
		log "checksums.txt has no entry for ${archive}"
		return 1
	fi

	actual="$(sha256_of "${TEMPORARY_DIRECTORY}/${archive}")" || {
		log "no sha256 tool available to verify ${archive}"
		return 1
	}

	# Case-insensitive compare, because sha256sum and openssl disagree on case.
	expected_lower="$(printf '%s' "${expected}" | tr 'A-F' 'a-f')"
	actual_lower="$(printf '%s' "${actual}" | tr 'A-F' 'a-f')"
	if [ "${expected_lower}" != "${actual_lower}" ]; then
		log "checksum mismatch for ${archive}"
		log "  expected ${expected_lower}"
		log "  actual   ${actual_lower}"
		rm -f "${TEMPORARY_DIRECTORY}/${archive}"
		return 1
	fi

	mkdir -p "${TEMPORARY_DIRECTORY}/unpacked" || return 1
	tar -xzf "${TEMPORARY_DIRECTORY}/${archive}" -C "${TEMPORARY_DIRECTORY}/unpacked" || {
		log "could not unpack ${archive}"
		return 1
	}
	if [ ! -f "${TEMPORARY_DIRECTORY}/unpacked/${BINARY}" ]; then
		log "${archive} does not contain ${BINARY}"
		return 1
	fi

	place "${TEMPORARY_DIRECTORY}/unpacked/${BINARY}" || return 1
	log "installed ${DESTINATION} from the verified prebuilt for ${target}"
}

# Builds from source. The last resort, and the only path that can leave the
# install with nothing — herdr installs no toolchains, so if cargo is missing
# there is nothing else to try.
build_from_source() {
	log "falling back to a source build"
	if ! command -v cargo >/dev/null 2>&1; then
		log ""
		log "cargo is not installed, and no verified prebuilt was available."
		log "Install Rust from https://rustup.rs and reinstall the plugin, or"
		log "open an issue at https://github.com/${REPO}/issues naming your"
		log "platform so a prebuilt can be published for it."
		return 1
	fi
	cargo build --release || {
		log "cargo build --release failed"
		return 1
	}
	place "target/release/${BINARY}" || return 1
	log "installed ${DESTINATION} from a source build"
}

# Moves a binary into ./bin/herdr-issues. Written to a sibling temporary name and
# renamed, so a half-written file is never left where the pane would run it.
place() {
	source_path="$1"
	mkdir -p bin || return 1
	staged="bin/.${BINARY}.staged.$$"
	cp -- "${source_path}" "${staged}" || return 1
	chmod 755 "${staged}" || return 1
	mv -f -- "${staged}" "${DESTINATION}" || return 1
}

main() {
	root="$(plugin_root)"
	cd "${root}" || exit 1

	if [ ! -f herdr-plugin.toml ]; then
		log "herdr-plugin.toml not found — run this from the plugin root"
		exit 1
	fi

	version="$(manifest_version)"
	if [ -z "${version}" ]; then
		log "could not read version from herdr-plugin.toml"
		version=""
	fi

	target=""
	if [ -n "${version}" ]; then
		target="$(detect_target)" || {
			log "no prebuilt for $(uname -s)/$(uname -m)"
			target=""
		}
	fi

	if [ -n "${target}" ]; then
		install_prebuilt "${version}" "${target}" && exit 0
	fi

	build_from_source || exit 1
}

main "$@"
