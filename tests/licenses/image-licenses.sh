#!/bin/sh
# Lists the licenses found in the worker images (docs/licenses.md):
#   - asir:      the OpenXM and Risa/Asir license files and the libraries of ox_asir
#   - macaulay2: the License fields of /usr/share/doc/<package>/copyright (Debian)
#   - oscar:     a classification of the license file of each Julia package and
#                binary artifact
# The classification of Julia licenses reads only the first license file and
# matches a few phrases, so it is a starting point for review, not a verdict.
#
#   sh tests/licenses/image-licenses.sh [ASIR_IMAGE] [M2_IMAGE] [OSCAR_IMAGE]
set -eu
export MSYS_NO_PATHCONV=1

ASIR="${1:-algebramesh-agent-asir:dev}"
M2="${2:-algebramesh-agent-macaulay2:dev}"
OSCAR="${3:-algebramesh-agent-oscar:dev}"

run() {
    image=$1
    shift
    docker run --rm --entrypoint sh "$image" -c "$*"
}

echo "== $ASIR"
run "$ASIR" '
    head -n 1 /etc/os-release
    echo "OpenXM/LICENSE: $(head -n 1 /OpenXM/LICENSE)"
    echo "OpenXM_contrib2/LICENSE: $(head -n 1 /OpenXM_contrib2/LICENSE)"
    echo "asir2018/LICENSE: $(sed -n 3p /OpenXM_contrib2/asir2018/LICENSE)"
    echo "asir2018/engine/A.c: $(sed -n 2p /OpenXM_contrib2/asir2018/engine/A.c)"
    echo "asir2018 C sources with the LIMITED USE header: $(grep -rl "FUJITSU LABORATORIES LIMITED" --include="*.[ch]" /OpenXM_contrib2/asir2018 | wc -l) of $(find /OpenXM_contrib2/asir2018 -name "*.[ch]" | wc -l)"
    echo "PARI (used by ox_pari): $(ls -d /OpenXM/src/pari/work/pari-* 2>/dev/null | xargs -n 1 basename), $(sed -n 1p /OpenXM/src/pari/work/pari-*/COPYING | sed "s/^ *//")"
    echo "libraries of ox_asir:"
    ldd /OpenXM/bin/ox_asir | sed "s/ (.*//"
'

echo "== $M2"
run "$M2" '
    head -n 1 /etc/os-release
    dpkg-query -W -f="\${Package} \${Version}\n" | while read -r pkg version; do
        f=/usr/share/doc/$pkg/copyright
        licenses=$( [ -f "$f" ] && grep -E "^License:" "$f" | sed "s/^License: *//" | sort -u | tr "\n" ";" )
        echo "$pkg $version: ${licenses:-(no machine-readable copyright)}"
    done
'

echo "== $OSCAR"
run "$OSCAR" '
    head -n 1 /etc/os-release
    julia --version
    classify() {
        t=$(head -c 6000 "$1" | tr "\n" " ")
        case "$t" in
            *"GNU AFFERO"*) echo AGPL ;;
            *"GNU LESSER GENERAL PUBLIC LICENSE"*|*"GNU Lesser General Public"*) echo LGPL ;;
            *"GNU GENERAL PUBLIC LICENSE"*"Version 3"*|*"GPL-3"*|*"GPLv3"*) echo GPL-3 ;;
            *"GNU GENERAL PUBLIC LICENSE"*"Version 1,"*) echo GPL-1 ;;
            *"GNU GENERAL PUBLIC LICENSE"*|*"GNU General Public License"*) echo GPL-2 ;;
            *"Mozilla Public License"*) echo MPL-2.0 ;;
            *"CeCILL-C"*) echo CeCILL-C ;;
            *"Artistic License 2.0"*) echo Artistic-2.0 ;;
            *"Apache License"*) echo Apache-2.0 ;;
            *"Permission is hereby granted, free of charge"*) echo MIT ;;
            *"Redistribution and use in source and binary forms"*) echo BSD ;;
            *"Permission to use, copy, modify"*) echo ISC-like ;;
            *) echo "unclassified" ;;
        esac
    }
    cd /opt/julia-depot/packages
    for d in */; do
        p=${d%/}
        f=$(ls "$p"/*/LICENSE* "$p"/*/COPYING* 2>/dev/null | head -n 1)
        echo "package $p: $( [ -n "$f" ] && classify "$f" || echo "no license file" )"
    done
    cd /opt/julia-depot/artifacts
    for l in */share/licenses/*; do
        [ -d "$l" ] || continue
        echo "artifact $(basename "$l"): $(classify "$(ls "$l"/* | head -n 1)") ($(ls "$l" | tr "\n" " "))"
    done | sort -u
'
