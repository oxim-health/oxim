# Mirth Connect migration report

Imported: server configuration backup (Mirth Connect 3.9.1)

| Result | Items |
|---|---|
| converted | 20 |
| approximated | 5 |
| unsupported | 5 |

Converted elements behave as in Mirth Connect. Approximated elements were converted with the difference described. Unsupported elements were left out and need attention before the channel goes live.

## Channels

| Mirth channel | OXIM channel | File | Converted | Approximated | Unsupported |
|---|---|---|---|---|---|
| Lab Feed | `lab-feed` | `lab-feed.yaml` | 5 | 2 | 0 |
| Lab Feed | `lab-feed-2` | `lab-feed-2.yaml` | 11 | 3 | 0 |
| Warehouse Import | `warehouse-import` | `warehouse-import.yaml.draft` (draft) | 2 | 0 | 2 |

## Server-wide elements

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | code template library "Validation" | 1 function template(s) and 0 compiled code block(s); each script step includes the templates it needs | `/serverConfiguration/codeTemplateLibraries/codeTemplateLibrary` |
| unsupported | global preprocessor script | global scripts are not supported; move the logic into transformer `script` steps of the channels | `/serverConfiguration/globalScripts/entry[3]` |
| converted | configuration map | 2 value(s) are substituted where connector settings use ${name} | `/serverConfiguration/configurationMap` |
| unsupported | alerts (1) | Mirth alerts are not imported; configure OXIM alerts | `/serverConfiguration/alerts` |
| unsupported | users (1) | users and their passwords are not imported; create OXIM accounts | `/serverConfiguration/users` |

## Channel `lab-feed` (Mirth: Lab Feed)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Lab Feed" | OXIM channel `lab-feed` in lab-feed.yaml | `/serverConfiguration/channels/channel[1]` |
| converted | source connector (File Reader) | OXIM file source reading /var/lib/lab/inbox | `/serverConfiguration/channels/channel[1]/sourceConnector/properties` |
| converted | source filter rule "Non-empty" (JavaScriptRule) | runs in the OXIM `script` step in Mirth compatibility mode; code templates included: isValid | `/serverConfiguration/channels/channel[1]/sourceConnector/filter/elements/com.mirth.connect.plugins.javascriptrule.JavaScriptRule` |
| approximated | destination "LIS REST" (HTTP Sender) | the template "{\\"data\\": \\"${message.encodedData}\\"}" is not applied; OXIM sends the encoded message (use a transformer or encoder to build other content) | `/serverConfiguration/channels/channel[1]/destinationConnectors/connector/properties/content` |
| converted | destination "LIS REST" (HTTP Sender) | OXIM http destination sending PUT requests to https://lis.example.org/api/results | `/serverConfiguration/channels/channel[1]/destinationConnectors/connector/properties` |
| approximated | destination "LIS REST" (HTTP Sender) | retrying without a pause became retrying every second | `/serverConfiguration/channels/channel[1]/destinationConnectors/connector/properties/destinationConnectorProperties` |
| converted | destination "LIS REST" (HTTP Sender) | queued durably and retried every 1s until delivered | `/serverConfiguration/channels/channel[1]/destinationConnectors/connector/properties/destinationConnectorProperties` |

## Channel `lab-feed-2` (Mirth: Lab Feed)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Lab Feed" | OXIM channel `lab-feed-2` in lab-feed-2.yaml | `/serverConfiguration/channels/channel[2]` |
| converted | source connector (TCP Listener) | OXIM mllp source listening on 0.0.0.0:6662 | `/serverConfiguration/channels/channel[2]/sourceConnector/properties` |
| converted | source filter rule "Chemistry" (RuleBuilderRule) | part of one `condition` filter that joins the rules with AND and OR as Mirth does | `/serverConfiguration/channels/channel[2]/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule[1]` |
| converted | source filter rule "Urgent" (RuleBuilderRule) | part of one `condition` filter that joins the rules with AND and OR as Mirth does | `/serverConfiguration/channels/channel[2]/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule[2]` |
| converted | source filter rule "Or hematology" (RuleBuilderRule) | part of one `condition` filter that joins the rules with AND and OR as Mirth does | `/serverConfiguration/channels/channel[2]/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule[3]` |
| converted | destination "Destination 1" (Channel Writer) | OXIM channel destination to the imported channel warehouse-import | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[1]/properties` |
| converted | destination "Destination 1" (Channel Writer) | OXIM queues every message durably; like Mirth without a queue, delivery gives up after 1 attempt(s), 10s apart | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[1]` |
| converted | destination "Destination 1" (TCP Sender) | OXIM tcp destination sending to 10.0.0.30:9100 | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[2]/properties` |
| converted | destination "Destination 1" (TCP Sender) | OXIM queues every message durably; like Mirth without a queue, delivery gives up after 3 attempt(s), 1500ms apart | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[2]/properties/destinationConnectorProperties` |
| approximated | destination "Destination 1" (File Writer) | appending messages to one file is not supported; each message is written to its own file named with the message identifier | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[3]/properties` |
| converted | destination "Destination 1" (File Writer) | OXIM file destination writing to /var/lib/lab/journal | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[3]/properties` |
| converted | destination "Destination 1" (File Writer) | OXIM queues every message durably; like Mirth without a queue, delivery gives up after 1 attempt(s), 10s apart | `/serverConfiguration/channels/channel[2]/destinationConnectors/connector[3]` |
| approximated | destination chain | Mirth waits for the previous destination before "Destination 1", "Destination 1"; OXIM delivers every destination from its own durable queue, so a destination cannot use an earlier destination's response and the order between destinations is not kept | `/serverConfiguration/channels/channel[2]/destinationConnectors` |
| approximated | source response (None) | Mirth sent no response; OXIM always acknowledges HL7 messages received over MLLP after storing them | `/serverConfiguration/channels/channel[2]/sourceConnector/properties/sourceConnectorProperties/responseVariable` |

## Channel `warehouse-import` (Mirth: Warehouse Import)

This channel was written as a draft (`.yaml.draft`) because its source connector has no OXIM equivalent yet. OXIM does not load it until the source is replaced and the file renamed to `.yaml`.

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Warehouse Import" | OXIM channel `warehouse-import` in warehouse-import.yaml.draft, disabled as in Mirth | `/serverConfiguration/channels/channel[3]` |
| unsupported | source connector (Database Reader) | OXIM has no source connector for Database Reader (DatabaseReceiverProperties) yet; the channel was written as a draft | `/serverConfiguration/channels/channel[3]/sourceConnector` |
| converted | source transformer step "Wrap" (JavaScriptStep) | runs in the OXIM `script` step in Mirth compatibility mode | `/serverConfiguration/channels/channel[3]/sourceConnector/transformer/elements/com.mirth.connect.plugins.javascriptstep.JavaScriptStep` |
| unsupported | destination "Out" (File Writer) | writing files over SFTP is not supported yet (planned: FTP, SFTP, SMB, S3); the destination was left out | `/serverConfiguration/channels/channel[3]/destinationConnectors/connector/properties` |

