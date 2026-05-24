---
name: ue-logging
description: Inspect saved Unreal Engine logs with RTK Unreal-aware filtering, including AutomationHost.log, UAT, UBT, UnrealEditor, Saved/Logs, Saved/Crashes, and Unreal project or plugin .log files.
---

# UE Logging

Use this skill when Codex needs to inspect saved Unreal Engine log files. This includes `AutomationHost.log`, UAT logs, UBT logs, `UnrealEditor` logs, logs under `Saved/Logs` or `Saved/Crashes`, and `.log` files produced while working on Unreal Engine projects or plugins.

## Command Choice

- For an unknown saved Unreal Engine log, prefer `rtk unreal automation cat <path>`.
- When the context clearly identifies a build log, prefer the corresponding RTK Unreal build mode.
- When the context clearly identifies a cook, package, commandlet, or automation-test log, prefer the corresponding RTK Unreal mode.
- If the user explicitly asks for raw log output, honor that request with a raw or generic log-reading path.

## Avoid

- Do not use `rtk cat`, `rtk read`, or raw `cat` for large saved Unreal Engine logs unless the user explicitly asks for raw output.
- Do not add or assume a Codex hook rewrite for log reads. This skill only guides manual command selection.
- Do not treat unrelated `.log` files as Unreal logs unless the project, plugin, filename, or path context clearly ties them to Unreal Engine.
