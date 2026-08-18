#!/usr/bin/env bash
# Wrapper kept for convenience; the real work is in fetch_osm.py, which chunks
# the query and caches each piece so a timeout does not cost the whole fetch.
exec python3 "$(dirname "$0")/fetch_osm.py" "$@"
