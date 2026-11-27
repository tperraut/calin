#!/bin/sh
# Links this home repo's agent material into Claude Code's global configuration,
# so that its skills, agents, commands and instructions load in every session,
# whatever directory the agent starts in.
#
# This is a starting point written by `calin home init`: edit it to suit your
# setup. As written it follows these rules:
#   - It only creates symbolic links, from ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/<item>
#     to <this home repo>/<item>, for each item in ITEMS below.
#   - Anything already at a target that is not a link into this home repo is
#     left alone and reported. Nothing is ever moved or deleted.
#   - An item this home repo does not have is skipped without a word.
#   - --dry-run prints what it would do and changes nothing.
#   - --unlink removes only the links that point into this home repo.
#   - It prints one line per item it looks at.
#
# Usage: scripts/link-claude.sh [--dry-run] [--unlink]

set -eu

ITEMS="skills agents commands CLAUDE.md"

usage() {
    echo "usage: $0 [--dry-run] [--unlink]"
}

dry_run=0
action=link
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=1 ;;
        --unlink) action=unlink ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $arg" >&2
            usage >&2
            exit 2
            ;;
    esac
done

# The home repo is the folder above this script's, with symbolic links resolved.
home_repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
config_dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}

# True when the link at $1 points into this home repo.
points_here() {
    target=$(readlink -- "$1") || return 1
    case "$target" in
        "$home_repo" | "$home_repo"/*) return 0 ;;
        *) return 1 ;;
    esac
}

link_item() {
    source=$home_repo/$1
    dest=$config_dir/$1
    [ -e "$source" ] || [ -L "$source" ] || return 0
    if [ -L "$dest" ]; then
        if [ "$(readlink -- "$dest")" = "$source" ]; then
            echo "linked already: $dest"
        else
            echo "left alone: $dest is a link to $(readlink -- "$dest")"
        fi
        return 0
    fi
    if [ -e "$dest" ]; then
        echo "left alone: $dest exists and is not a link"
        return 0
    fi
    if [ "$dry_run" = 1 ]; then
        echo "would link: $dest -> $source"
        return 0
    fi
    mkdir -p -- "$config_dir"
    ln -s -- "$source" "$dest"
    echo "linked: $dest -> $source"
}

unlink_item() {
    dest=$config_dir/$1
    if [ -L "$dest" ] && points_here "$dest"; then
        if [ "$dry_run" = 1 ]; then
            echo "would unlink: $dest"
        else
            rm -- "$dest"
            echo "unlinked: $dest"
        fi
    elif [ -L "$dest" ] || [ -e "$dest" ]; then
        echo "left alone: $dest is not a link into $home_repo"
    else
        echo "not linked: $dest"
    fi
}

for item in $ITEMS; do
    "${action}_item" "$item"
done
