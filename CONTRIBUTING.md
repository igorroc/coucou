# Contributing to Navi Assistant

Thanks for wanting to help Navi grow up! 🫶

## Getting started

```bash
brew install xcodegen
cd NaviAssistant && xcodegen && open NaviAssistant.xcodeproj
```

Never edit `NaviAssistant.xcodeproj` by hand: change `project.yml` and run `xcodegen`.

## Good first contributions

- A new integration (a poller + a pill + a detail card). Look at `StripePoller.swift` for a compact example.
- A new emote or sound for Navi.
- Bug fixes — please describe how to reproduce.

## Rules of the house

- Swift 6, SwiftUI + AppKit, **no third-party dependencies** unless there's really no other way.
- Secrets go in the Keychain, never on disk or in git.
- No telemetry, no network calls except to services the user configured.
- Never block Claude Code: if the app doesn't answer, the hook must exit right away.
- Never write `~/.claude/settings.json` without a backup and the user's confirmation.
- Keep it light: 0 % CPU when the island is hidden.

## Pull requests

- One topic per PR, with a short GIF or screenshot for anything visual.
- Build must pass with no new warnings.
