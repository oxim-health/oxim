# Mirth Connect migration report

Imported: channel export (Mirth Connect 4.4.0)

| Result | Items |
|---|---|
| converted | 13 |
| approximated | 12 |
| unsupported | 4 |

Converted elements behave as in Mirth Connect. Approximated elements were converted with the difference described. Unsupported elements were left out and need attention before the channel goes live.

## Channels

| Mirth channel | OXIM channel | File | Converted | Approximated | Unsupported |
|---|---|---|---|---|---|
| Lab Results: Analyzer -> LIS | `lab-results-analyzer-lis` | `lab-results-analyzer-lis.yaml` | 12 | 12 | 4 |

## Server-wide elements

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | code template library "Name utilities" | 3 function template(s) and 0 compiled code block(s); each script step includes the templates it needs | `/channel/exportData/codeTemplateLibraries/codeTemplateLibrary` |

## Channel `lab-results-analyzer-lis` (Mirth: Lab Results: Analyzer -> LIS)

| Result | Element | Detail | Location |
|---|---|---|---|
| converted | channel "Lab Results: Analyzer -> LIS" | OXIM channel `lab-results-analyzer-lis` in lab-results-analyzer-lis.yaml | `/channel` |
| approximated | source connector (TCP Listener) | the character set windows-1254 is not configured per connector in OXIM; HL7 v2 messages use MSH-18 and other data UTF-8 unless the channel format sets an encoding | `/channel/sourceConnector/properties/charsetEncoding` |
| converted | source connector (TCP Listener) | OXIM tcp source listening on 0.0.0.0:5200 | `/channel/sourceConnector/properties` |
| converted | source filter | runs in the OXIM `script` step in Mirth compatibility mode; the rules are joined in one script because they mix OR with JavaScript | `/channel/sourceConnector/filter` |
| converted | source filter rule "Results" (RuleBuilderRule) | part of the combined filter script | `/channel/sourceConnector/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule` |
| converted | source filter rule "Or QC flagged" (JavaScriptRule) | part of the combined filter script | `/channel/sourceConnector/filter/elements/com.mirth.connect.plugins.javascriptrule.JavaScriptRule` |
| converted | source transformer step "Normalize name" (JavaScriptStep) | runs in the OXIM `script` step in Mirth compatibility mode; code templates included: formatName, capitalize | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.javascriptstep.JavaScriptStep[1]` |
| converted | source transformer step "cleanId" (MapperStep) | runs in the OXIM `script` step in Mirth compatibility mode; rebuilt as the JavaScript Mirth generates | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.mapper.MapperStep[1]` |
| approximated | source transformer step "lastAnalyzer" (MapperStep) | runs in the OXIM `script` step in Mirth compatibility mode; the value goes to Mirth's globalMap, which is shared between messages; verify the behavior | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.mapper.MapperStep[2]` |
| unsupported | source transformer step "Stylesheet" (XsltStep) | XSLT steps are not supported; the step was left out | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.xsltstep.XsltStep` |
| approximated | source transformer step "Route copy" (JavaScriptStep) | runs in the OXIM `script` step in Mirth compatibility mode; review the script: it uses router., which OXIM scripts may not provide | `/channel/sourceConnector/transformer/elements/com.mirth.connect.plugins.javascriptstep.JavaScriptStep[2]` |
| converted | destination "LIS" (TCP Sender) | OXIM mllp destination sending to lis.example.org:2575 | `/channel/destinationConnectors/connector[1]/properties` |
| unsupported | destination "LIS" (TCP Sender) response transformer | response transformers are not supported; the steps were left out | `/channel/destinationConnectors/connector[1]/responseTransformer` |
| converted | destination "LIS" (TCP Sender) | queued durably and retried every 30s until delivered | `/channel/destinationConnectors/connector[1]/properties/destinationConnectorProperties` |
| approximated | destination "LIS" (TCP Sender) | several queue threads became one: each OXIM destination queue is delivered in order | `/channel/destinationConnectors/connector[1]/properties/destinationConnectorProperties` |
| approximated | destination "Portal" (HTTP Sender) | credentials were not copied; add an `Authorization` header to the destination | `/channel/destinationConnectors/connector[2]/properties` |
| converted | destination "Portal" (HTTP Sender) | OXIM http destination sending POST requests to https://portal.example.org/api/results?format=hl7%20v2 | `/channel/destinationConnectors/connector[2]/properties` |
| converted | destination "Portal" filter rule "Final results only" (RuleBuilderRule) | declarative OXIM filter | `/channel/destinationConnectors/connector[2]/filter/elements/com.mirth.connect.plugins.rulebuilder.RuleBuilderRule` |
| approximated | destination "Portal" (HTTP Sender) transformer | Mirth serializes the message as XML after this transformer; OXIM keeps the HL7V2 message, so use an encoder or a script step to change the format | `/channel/destinationConnectors/connector[2]/transformer` |
| converted | destination "Portal" (HTTP Sender) | OXIM queues every message durably; like Mirth without a queue, delivery gives up after 4 attempt(s), 10s apart | `/channel/destinationConnectors/connector[2]/properties/destinationConnectorProperties` |
| unsupported | destination "Warehouse" (Database Writer) | OXIM has no destination connector for Database Writer (DatabaseDispatcherProperties) yet; the destination was left out | `/channel/destinationConnectors/connector[3]` |
| approximated | destination chain | Mirth waits for the previous destination before "Portal"; OXIM delivers every destination from its own durable queue, so a destination cannot use an earlier destination's response and the order between destinations is not kept | `/channel/destinationConnectors` |
| approximated | source response (d1) | this response is not supported; OXIM acknowledges after storing the message | `/channel/sourceConnector/properties/sourceConnectorProperties/responseVariable` |
| unsupported | channel preprocessor script | channel preprocessor scripts are not supported; move the logic into a transformer `script` step | `/channel/preprocessingScript` |
| approximated | initial state STOPPED | OXIM has no deployed-but-stopped state; the channel is written disabled | `/channel/properties` |
| approximated | remove content on completion | OXIM keeps message content until `retention.contents_after` in oxim.yaml | `/channel/properties` |
| approximated | custom metadata columns | custom metadata columns are not supported; their values are not stored | `/channel/properties` |
| approximated | pruning settings | OXIM retention is set for the whole engine (`retention` in oxim.yaml) | `/channel/exportData/metadata/pruningSettings` |

