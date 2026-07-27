#!/bin/sh
#
# Fail unless a release tag agrees with the version the plugin declares.
#
# `scripts/fetch-or-build.sh` keys the download URL off the manifest `version`,
# so a release tagged v0.2.0 while the manifest still says 0.1.0 publishes
# artifacts nobody will ever ask for: every install looks for v0.1.0, finds
# nothing, and silently takes the multi-minute source-build path. That failure is
# invisible from the release page, which is why it is a CI gate rather than a
# convention (ADR-0003).
#
# Usage: scripts/check-release-version.sh v0.1.0
#
# The manifest is the binding version. Cargo.toml is checked too, because a
# manifest and a crate that disagree are a bug on their own.

set -eu

usage() {
	printf 'usage: %s <tag>\n' "$0" >&2
}

if [ "$#" -ne 1 ]; then
	usage
	exit 2
fi

tag="$1"
case "${tag}" in
v*) ;;
*)
	printf 'tag %s does not start with "v"\n' "${tag}" >&2
	exit 1
	;;
esac
tag_version="${tag#v}"

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
cd "${root}"

# Anchored, so `min_herdr_version` does not match; first-wins, so a later table
# cannot shadow the top-level key.
manifest_version="$(
	sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' herdr-plugin.toml |
		head -n 1
)"

# Only the `[package]` version, so a pinned dependency version is never read.
cargo_version="$(
	sed -n '/^\[package\]/,/^\[/ s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml |
		head -n 1
)"

status=0

if [ -z "${manifest_version}" ]; then
	printf 'could not read version from herdr-plugin.toml\n' >&2
	status=1
elif [ "${manifest_version}" != "${tag_version}" ]; then
	printf 'tag %s disagrees with herdr-plugin.toml version %s\n' \
		"${tag}" "${manifest_version}" >&2
	printf 'the install script downloads v%s, so this release would be unreachable\n' \
		"${manifest_version}" >&2
	status=1
fi

if [ -z "${cargo_version}" ]; then
	printf 'could not read version from Cargo.toml\n' >&2
	status=1
elif [ "${cargo_version}" != "${tag_version}" ]; then
	printf 'tag %s disagrees with Cargo.toml version %s\n' \
		"${tag}" "${cargo_version}" >&2
	status=1
fi

if [ "${status}" -eq 0 ]; then
	printf 'tag %s agrees with herdr-plugin.toml and Cargo.toml\n' "${tag}"
fi

exit "${status}"
