{{/* Chart name. */}}
{{- define "oxim.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Full name: release plus chart name, unless the release name contains it. */}}
{{- define "oxim.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "oxim.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "oxim.labels" -}}
helm.sh/chart: {{ include "oxim.chart" . }}
{{ include "oxim.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "oxim.selectorLabels" -}}
app.kubernetes.io/name: {{ include "oxim.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "oxim.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "oxim.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "oxim.image" -}}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}

{{- define "oxim.secretName" -}}
{{- if .Values.secret.existingSecret }}
{{- .Values.secret.existingSecret }}
{{- else }}
{{- include "oxim.fullname" . }}
{{- end }}
{{- end }}

{{/* The engine configuration file. */}}
{{- define "oxim.config" -}}
data_dir: /var/lib/oxim
channels_dir: /etc/oxim/channels
tables_dir: /etc/oxim/tables
{{ toYaml .Values.config }}
{{- end }}
