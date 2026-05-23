---
name: nexus-chat
description: Human communication channel integration for real-time interaction across Slack, Discord, and WhatsApp.
---

# NEXUS Human Communication Skill

Use this skill to communicate with human operators through Slack, Discord, or WhatsApp channels.

## When to Use

- Major workflow events (started, assigned, completed, error)
- Ambiguity requiring human clarification
- Dangerous command approval requests
- Long-running operation status updates

## Supported Channels

NEXUS supports simultaneous multi-channel operation. Configure any combination of:

| Channel | Env Vars | Notes |
|---------|----------|-------|
| Slack | `NEXUS_CHAT_SLACK_BOT_TOKEN`, `NEXUS_CHAT_SLACK_CHANNEL_ID` | Full message formatting with blocks |
| Discord | `NEXUS_CHAT_DISCORD_BOT_TOKEN`, `NEXUS_CHAT_DISCORD_CHANNEL_ID` | Embed-based rich messages |
| WhatsApp | `NEXUS_CHAT_WHATSAPP_API_KEY`, `NEXUS_CHAT_WHATSAPP_PHONE_NUMBER` | Text-only via Cloud API |

## Message Templates

### workflow_started
"🚀 Starting ticket #123: Authentication fixes"

### agent_assigned
"👷 FORGE assigned to sign-up form changes (slot-7)"

### agent_completed
"✅ FORGE completed slot-7 in 12m"

### workflow_error
"❌ FORGE failed: 'TypeError: agent is undefined'"

### question_to_human
"🤔 Should we prioritize payment gateway or shipping setup?"

### approval_request
"⚠️ FORGE requests approval for `npm ci --force`"

## Human Commands

Human operators can send natural language messages through any configured channel. NEXUS interprets them via LLM into structured commands.

| Command | Action |
|---------|--------|
| `pause T-XXX` | Set ticket status to `AwaitingHuman` |
| `resume T-XXX` | Reset ticket to `Open` for re-assignment |
| `approve [worker]` | Approve pending dangerous command |
| `reject [worker]` | Reject pending dangerous command |
| `block worker-id [reason]` | Mark worker as blocked |
| `reroute from:to` | Reassign work to different worker |
| `answer T-XXX: response` | Provide answer to pending question |

Natural language messages that don't match command patterns are passed through LLM interpretation for semantic understanding.

## Configuration

Enable the chat integration via environment variables:

```bash
# Global settings
NEXUS_CHAT_ENABLED=true
NEXUS_CHAT_DEV_MODE=false  # Uses mock client when true

# Slack
NEXUS_CHAT_SLACK_BOT_TOKEN=xoxb-your-token
NEXUS_CHAT_SLACK_CHANNEL_ID=C12345678
NEXUS_CHAT_SLACK_SIGNING_SECRET=your-signing-secret

# Discord
NEXUS_CHAT_DISCORD_BOT_TOKEN=your-discord-bot-token
NEXUS_CHAT_DISCORD_CHANNEL_ID=123456789012345678

# WhatsApp (Cloud API)
NEXUS_CHAT_WHATSAPP_API_KEY=your-whatsapp-api-key
NEXUS_CHAT_WHATSAPP_PHONE_NUMBER=your-phone-number-id
NEXUS_CHAT_WHATSAPP_API_URL=https://graph.facebook.com/v18.0
```

## Dev Mode

When `NEXUS_CHAT_DEV_MODE=true`, the system uses a mock chat client that:
- Logs messages to stdout instead of real channels
- Allows injecting test commands programmatically
- Returns default responses for Q&A loops

## Rate Limiting

Human commands are rate-limited to 10 commands per 5 minutes per user to prevent spam from overwhelming the system.

## Architecture

- `HumanMessage` — raw message from any channel (user_id, channel_id, text, timestamp)
- `HumanCommand` — structured command interpreted from HumanMessage (command type, ticket_id, worker_id, payload)
- `ChatClient` trait — implemented by SlackClient, DiscordClient, WhatsAppClient, and MockChatClient
- `HumanChannel` — aggregates all configured clients, broadcasts notifications to all active channels
- Message polling runs every 2 seconds per active channel type
