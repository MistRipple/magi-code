#!/bin/bash
BASE="/Users/xie/code/magi-rust-rewrite"
echo "## Phase 8 (magi-api routes)"
if [ -d "$BASE/magi-api/src/routes" ]; then
    find "$BASE/magi-api/src/routes" -type f -name "*.rs" | while read -r f; do
        rel_path=${f#$BASE/}
        # simple check: if the file contains 'todo!()' or 'unimplemented!()', consider it a stub
        if grep -qE 'todo!\(\)|unimplemented!\(\)' "$f"; then
            echo "- $rel_path: STUB"
        else
            echo "- $rel_path: EXISTS (Implemented)"
        fi
    done
else
    echo "- magi-api/src/routes/: MISSING"
fi
