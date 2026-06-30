# Coder Compatibility

OpenFlows pins the Coder control-plane image in `docker-compose.yml` and treats the pin as the default startup target.

Current behavior:

- Default tag: `latest`
- Runtime fallback tags: `preview`
- Override the image tag with `CODER_IMAGE_TAG`

The startup path is self-healing:

- If port `7080` is already occupied, OpenFlows reuses a healthy nearby Coder instance or moves to the next free port.
- If the pinned image tag cannot be pulled, OpenFlows retries the fallback tags automatically before giving up.

If you want to force a specific image tag, set `CODER_IMAGE_TAG` in your `.env`.
