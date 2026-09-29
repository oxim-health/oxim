# Mirth Connect migration report

Imported: channel group export (Mirth Connect 3.12.0)

| Result | Items |
|---|---|
| converted | 6 |
| approximated | 1 |
| unsupported | 0 |

Converted elements behave as in Mirth Connect. Approximated elements were converted with the difference described. Unsupported elements were left out and need attention before the channel goes live.

## Channels

| Mirth channel | OXIM channel | File | Converted | Approximated | Unsupported |
|---|---|---|---|---|---|
| Glucose meters | `glucose-meters` | `glucose-meters.yaml` | 4 | 1 | 0 |
| Retired meters | `retired-meters` | `retired-meters.yaml` | 2 | 0 | 0 |

## Channel `glucose-meters` (Mirth: Glucose meters)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Glucose meters" | OXIM channel `glucose-meters` in glucose-meters.yaml | `/channelGroup/channels/channel[1]` |
| approximated | source connector (HTTP Listener) | OXIM accepts POST and PUT requests (set `methods` for others) and stores the raw body | `/channelGroup/channels/channel[1]/sourceConnector/properties` |
| converted | source connector (HTTP Listener) | OXIM http source listening on 0.0.0.0:8080 | `/channelGroup/channels/channel[1]/sourceConnector/properties` |
| converted | destination "Archive" (File Writer) | OXIM file destination writing to /var/lib/poc | `/channelGroup/channels/channel[1]/destinationConnectors/connector/properties` |
| converted | destination "Archive" (File Writer) | queued durably and retried every 60s until delivered | `/channelGroup/channels/channel[1]/destinationConnectors/connector/properties/destinationConnectorProperties` |

## Channel `retired-meters` (Mirth: Retired meters)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Retired meters" | OXIM channel `retired-meters` in retired-meters.yaml, disabled as in Mirth | `/channelGroup/channels/channel[2]` |
| converted | source connector (TCP Listener) | OXIM mllp source listening on 0.0.0.0:6700 | `/channelGroup/channels/channel[2]/sourceConnector/properties` |

