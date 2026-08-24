# Fix malformed hunk in latch-tmux patch 0002

`just release-cli` failed while applying `patches/tmux/0002-latch-deferred-parse.patch`:

```
patching file tmux.h
patch: **** malformed patch at line 26:  #define CLIENT_ASSUMEPASTING 0x2000000000ULL
```

The second hunk in `tmux.h` added a 10-line comment plus three `#define`s (16 new lines total) but the unified-diff header claimed `+2129,14`. BSD `patch` treats an over-long hunk as malformed rather than applying with fuzz. Corrected the header to `+2129,16` and updated the patch sha256 in `patches/tmux/manifest.json`.
