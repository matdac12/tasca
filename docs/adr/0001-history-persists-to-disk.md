# The History is saved to disk

Tasca v1 persisted nothing and listed clipboard history as out of scope. The History now survives Quit and reboots: it is saved under `%LOCALAPPDATA%\Tasca\history\` by a worker thread, keeping the newest 100 unpinned Clips plus every Pinned Clip, oldest unpinned first out. A history that a restart wipes is the main weakness of Windows' own Win+V history, and the reason to build this at all; 100 Clips keeps the store small while giving search something to find.

## Considered Options

- **Memory only:** lost on every reboot or Quit, like Win+V.
- **No limit:** grows forever, mostly with images.
