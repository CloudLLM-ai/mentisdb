# MentisDB 0.10.7.52 (hotfix)

**Date:** August 16, 2026

## Hotfix

0.10.6 panicked on boot against existing `mentisdb-skills.bin` files from 0.10.5:

```
Failed to deserialize skill registry: OtherString(
  "invalid value: integer `10633`, expected variant index 0 <= i < 2")
```

Cause: `SkillVersion.schema_version` was inserted before `content` in the bincode layout. The Full/Delta payload length was read as the enum discriminant.

0.10.7 decodes the 0.10.5 V2 layout and rewrites the file on open/migrate. Do not delete `mentisdb-skills.bin`.

## Upgrade

```bash
cargo install mentisdb --locked --force
```

Restart the daemon.

## Otherwise

Same as [0.10.6.51](https://github.com/CloudLLM-ai/mentisdb/releases/tag/0.10.6.51): permanent skill delete, sidecar WAL, persist wins.
