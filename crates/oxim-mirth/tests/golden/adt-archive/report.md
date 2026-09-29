# Mirth Connect migration report

Imported: channel export (Mirth Connect 3.12.0)

| Result | Items |
|---|---|
| converted | 14 |
| approximated | 0 |
| unsupported | 0 |

Converted elements behave as in Mirth Connect. Approximated elements were converted with the difference described. Unsupported elements were left out and need attention before the channel goes live.

## Channels

| Mirth channel | OXIM channel | File | Converted | Approximated | Unsupported |
|---|---|---|---|---|---|
| ADT Archive | `adt-archive` | `adt-archive.yaml` | 14 | 0 | 0 |

## Channel `adt-archive` (Mirth: ADT Archive)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "ADT Archive" | OXIM channel `adt-archive` in adt-archive.yaml | `/channel` |
| converted | source connector (TCP Listener) | OXIM mllp source listening on 127.0.0.1:6661 | `/channel/sourceConnector/properties` |
| converted | source filter rule "ADT or ORU" (RuleBuilderRule) | declarative OXIM filter | `/channel/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule[1]` |
| converted | source filter rule "Has a patient id" (RuleBuilderRule) | declarative OXIM filter | `/channel/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule[2]` |
| converted | source filter rule "Not a test system" (RuleBuilderRule) | declarative OXIM filter | `/channel/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule[3]` |
| converted | source transformer step "Set receiving facility" (MessageBuilderStep) | `map` operation writing MSH-6.1 | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.messagebuilder.MessageBuilderStep[1]` |
| converted | source transformer step "patientId" (MapperStep) | `map` operation storing the message variable `patientId` (channelMap in scripts) | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.mapper.MapperStep` |
| converted | source transformer step "Copy patient id" (MessageBuilderStep) | `map` operation writing PID-2.1 | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.messagebuilder.MessageBuilderStep[2]` |
| converted | source transformer step "Default sex" (MessageBuilderStep) | `map` operation writing PID-8 | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.messagebuilder.MessageBuilderStep[3]` |
| converted | source transformer step "Old logging" (JavaScriptStep) | disabled in Mirth; left out | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.javascriptstep.JavaScriptStep` |
| converted | destination "Archive" (File Writer) | OXIM file destination writing to /var/lib/oxim/archive | `/channel/destinationConnectors/connector[1]/properties` |
| converted | destination "Archive" (File Writer) | queued durably and retried every 5s until delivered | `/channel/destinationConnectors/connector[1]/properties/destinationConnectorProperties` |
| converted | destination "Old archive" (File Writer) | disabled in Mirth; left out | `/channel/destinationConnectors/connector[2]` |
| converted | source response (Auto-generate (After source transformer)) | OXIM acknowledges each message after storing it durably | `/channel/sourceConnector/properties/sourceConnectorProperties/responseVariable` |

