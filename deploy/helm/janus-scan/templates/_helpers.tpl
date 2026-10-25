{{- define "janus-scan.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "janus-scan.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "janus-scan.selector" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* server.yaml: destinations mounted at /nas/0, /nas/1, ... */}}
{{- define "janus-scan.config" -}}
destinations:
{{- range $i, $d := .Values.destinations }}
  - name: {{ required "every destination needs a name" $d.name | quote }}
    path: /nas/{{ $i }}
    {{- with $d.nameTemplate }}
    name_template: {{ . | quote }}
    {{- end }}
{{- else }} []
{{- end }}
{{- end -}}
