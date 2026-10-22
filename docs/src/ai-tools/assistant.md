# Assistant

An AI assistant can work in Klipp with you: read the open project and edit it, grade, mix, title, check and export, while you watch. Every change it makes shows in the app as it happens and is a single undo step.

## How It Connects

Klipp listens on a local control socket. **Klipp's MCP program** (`klipp-mcp`, installed next to Klipp) speaks the Model Context Protocol to an assistant and passes its requests to that socket. Nothing is reachable from outside the computer; the assistant program you connect decides where your conversation with it goes.

**Preferences → AI → Connect an assistant**:

1. Checks that Klipp's MCP program is installed next to Klipp.
2. **Connect** writes the configuration for a supported assistant; for any other, copy the configuration shown (the common `mcpServers` JSON form) into it.
3. **Check** confirms the assistant can reach Klipp.

## What to Ask

Ask in your own words. Some examples:

- *"Make a two-minute cut of this interview, keeping the best answers."*
- *"Take out the ums and long pauses."*
- *"Make a 30-second montage of the trip to this track, cutting on the beat."*
- *"Find the close-ups of the guitar."*
- *"Make a vertical version that keeps her face in shot."*
- *"Make these shots match the first one."* — *"Give it a warm film look."*
- *"Add lower thirds with their names."* — *"Make captions and translate them to Spanish."*
- *"Check it before I export, and fix what you can."*

Ask the assistant what it can do in Klipp: it answers from Klipp's own guide, with a first step that fits what is open.

## Working Together

- **Edit Plan** (**Window → Edit Plan**) — for a larger job the assistant can propose a plan of edits; you read it, change it and approve it before anything is cut.
- **Delivery Check** (**Window → Delivery Check**) — the same check the assistant runs, from the app; a finding you accept is kept with your reason.
- **Brief** — a sequence can carry a brief of what the piece is for, which the assistant and the check both work to.
- **Downloads** — some tools need a model or a library download the first time; the assistant asks before starting one.

## Undo and Safety

Each assistant change is one undo step in the same history as your own edits; a batch of changes it makes together is one step. It works on the same open project you see, so you can stop, undo or carry on by hand at any point.
