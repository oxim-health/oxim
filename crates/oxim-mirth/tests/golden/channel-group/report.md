# Mirth Connect migration report

Imported: channel group export (Mirth Connect 3.12.0)

| Result | Items |
|---|---|
| converted | 5 |
| approximated | 0 |
| unsupported | 1 |

Converted elements behave as in Mirth Connect. Approximated elements were converted with the difference described. Unsupported elements were left out and need attention before the channel goes live.

## Channels

| Mirth channel | OXIM channel | File | Converted | Approximated | Unsupported |
|---|---|---|---|---|---|
| Glucose meters | `glucose-meters` | `glucose-meters.yaml.draft` (draft) | 3 | 0 | 1 |
| Retired meters | `retired-meters` | `retired-meters.yaml` | 2 | 0 | 0 |

## Channel `glucose-meters` (Mirth: Glucose meters)

This channel was written as a draft (`.yaml.draft`) because its source connector has no OXIM equivalent yet. OXIM does not load it until the source is replaced and the file renamed to `.yaml`.

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Glucose meters" | OXIM channel `glucose-meters` in glucose-meters.yaml.draft | `/channelGroup/channels/channel[1]` |
| unsupported | source connector (HTTP Listener) | OXIM has no source connector for HTTP Listener (HttpReceiverProperties) yet; the channel was written as a draft | `/channelGroup/channels/channel[1]/sourceConnector` |
| converted | destination "Archive" (File Writer) | OXIM file destination writing to /var/lib/poc | `/channelGroup/channels/channel[1]/destinationConnectors/connector/properties` |
| converted | destination "Archive" (File Writer) | queued durably and retried every 60s until delivered | `/channelGroup/channels/channel[1]/destinationConnectors/connector/properties/destinationConnectorProperties` |

## Channel `retired-meters` (Mirth: Retired meters)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Retired meters" | OXIM channel `retired-meters` in retired-meters.yaml, disabled as in Mirth | `/channelGroup/channels/channel[2]` |
| converted | source connector (TCP Listener) | OXIM mllp source listening on 0.0.0.0:6700 | `/channelGroup/channels/channel[2]/sourceConnector/properties` |

