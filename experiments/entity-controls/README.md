# Entity-control media probe

Run the bounded Home Assistant media API probe with:

```sh
uv run --locked python experiments/entity-controls/qualify.py
```

It starts one disposable container using the pinned Home Assistant image
`ghcr.io/home-assistant/home-assistant@sha256:c6c1ca9931036bf3a2c4cc088d8db25dfea3b33935a5ecc83c40349462e2f69e`, enables Home Assistant's demo integration, creates a temporary media file, then removes the container and its named volume. It does not connect to any configured or household Home Assistant instance. The JSON report omits access credentials and media-content IDs.

The 2026-11-14 probe run passed. It confirmed that Home Assistant exposes a required `media` selector in the UI service metadata while `call_service` accepts flattened `media_content_type` and `media_content_id` fields. `media_play`, `volume_set`, `volume_mute`, and `play_media` succeeded; providing both `enqueue` and `announce` failed with `invalid_format`. The browse/search WebSocket queries validated paired content IDs and media classes; browsing returned a root with seven sources, nested browsing returned the seeded local media folder, and the search query succeeded. Container and volume cleanup both succeeded.
