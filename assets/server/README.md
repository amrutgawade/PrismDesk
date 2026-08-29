# Pinned device server

| Field | Value |
|---|---|
| File | `scrcpy-server-v3.3.1.jar` |
| Version | scrcpy **3.3.1** (stock, unmodified) |
| Source | `C:\Program Files\scrcpy\scrcpy-server` (Genymobile scrcpy, Apache-2.0) |
| SHA-256 | `a0f70b20aa4998fbf658c94118cd6c8dab6abbb0647a3bdab344d70bc1ebcbb8` |
| Size | 90788 bytes |

This is the **stock** scrcpy 3.3.1 server used for Phase 0/1 to prove the pipe.
Per the architecture plan it will be replaced by our **pinned fork** (still
Apache-2.0) that adds: MediaCodec low-latency mode, a client→server
request-keyframe control message (on-demand IDR for OBS/reconnect attach), and
UHID input. The client hard-matches this version string in its handshake.

The `com.genymobile.scrcpy.Server` entry point is launched on-device via
`app_process` as the shell UID after `adb push` to `/data/local/tmp`.
