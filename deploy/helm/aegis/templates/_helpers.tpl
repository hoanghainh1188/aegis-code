{{/* At most 40: a StatefulSet's name, with its suffix here and the hash
     Kubernetes adds, must fit a 63-character label. */}}
{{- define "aegis.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 40 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 40 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "aegis.workerNamespace" -}}
{{- default (printf "%s-worker" .Release.Namespace) .Values.worker.namespace.name -}}
{{- end -}}

{{- define "aegis.image" -}}
{{- printf "%s/%s:%s" .Values.image.registry .Values.image.repository (default .Chart.AppVersion .Values.image.tag) -}}
{{- end -}}

{{- define "aegis.labels" -}}
app.kubernetes.io/name: {{ .Chart.Name }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{/* Selector labels; call with (list $ "api"). */}}
{{- define "aegis.selector" -}}
{{- $root := index . 0 -}}
app.kubernetes.io/name: {{ $root.Chart.Name }}
app.kubernetes.io/instance: {{ $root.Release.Name }}
app.kubernetes.io/component: {{ index . 1 }}
{{- end -}}

{{/* The aegis container's user: ADR 0008 §1. The gate starts sandboxes
     with the same uid:gid. */}}
{{- define "aegis.containerSecurity" -}}
runAsNonRoot: true
runAsUser: 10001
runAsGroup: 10001
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
capabilities:
  drop: [ALL]
seccompProfile:
  type: RuntimeDefault
{{- end -}}

{{/* OnRootMismatch: a volume is not walked file by file at each start. */}}
{{- define "aegis.podSecurity" -}}
fsGroup: 10001
fsGroupChangePolicy: OnRootMismatch
{{- end -}}

{{- define "aegis.configVolume" -}}
- name: config
  configMap:
    name: {{ include "aegis.fullname" . }}-config
    items:
      - {key: server.toml, path: server.toml}
      {{- range $file, $_ := .Values.config.modeFiles }}
      - {key: {{ printf "mode.%s" $file | quote }}, path: {{ printf "modes/%s" $file | quote }}}
      {{- end }}
{{- end -}}

{{/* A hash in the Pods' annotations: a new configuration rolls them. */}}
{{- define "aegis.configHash" -}}
{{- include "aegis.configData" . | sha256sum -}}
{{- end -}}

{{/* The ConfigMap's data. In TOML: strings with quote, integers with int. */}}
{{- define "aegis.configData" -}}
{{- $c := .Values.config -}}
server.toml: |
  [secrets]
  source = "file"
  dir = "/run/secrets"
  refresh_s = {{ int $c.secretsRefreshSeconds }}

  [modes]
  {{- range $name, $path := $c.modes }}
  {{ $name }} = {{ $path | quote }}
  {{- end }}

  [api]
  listen = "0.0.0.0:8080"

  [metrics]
  listen = "0.0.0.0:9464"

  [auth]
  public_url = {{ required "config.auth.publicUrl is required" $c.auth.publicUrl | quote }}
  session_hours = {{ int $c.auth.sessionHours }}
  api_requests_per_minute = {{ int $c.auth.apiRequestsPerMinute }}
  api_failures_per_minute = {{ int $c.auth.apiFailuresPerMinute }}
  trusted_proxies = {{ if kindIs "invalid" $c.auth.trustedProxies }}{{ ternary 1 0 .Values.ingress.enabled }}{{ else }}{{ int $c.auth.trustedProxies }}{{ end }}
  {{- if $c.auth.oidc.issuer }}

  [auth.oidc]
  issuer = {{ $c.auth.oidc.issuer | quote }}
  client_id = {{ $c.auth.oidc.clientId | quote }}
  {{- end }}

  [budget]
  time_zone = {{ $c.budget.timeZone | quote }}
  warn_ratio = {{ printf "%.2f" (float64 $c.budget.warnRatio) }}

  [worker]
  max_workers = {{ int $c.worker.maxWorkers }}
  max_cost_usd = {{ printf "%.2f" (float64 $c.worker.maxCostUsd) }}
  lease_s = {{ int $c.worker.leaseSeconds }}
  renew_s = {{ int $c.worker.renewSeconds }}
  poll_s = {{ int $c.worker.pollSeconds }}
  drain_s = {{ int $c.worker.drainSeconds }}
  retry_base_s = {{ int $c.worker.retryBaseSeconds }}

  [trackers]
  poll_s = {{ int $c.trackers.pollSeconds }}

  [sonar]
  check_s = {{ int $c.sonar.checkSeconds }}

  [reviews]
  max_rounds = {{ int $c.reviews.maxRounds }}
  {{- with $c.extraToml }}

  {{- . | nindent 2 }}
  {{- end }}
{{- range $file, $text := $c.modeFiles }}
{{ printf "mode.%s" $file | quote }}: |
  {{- $text | nindent 2 }}
{{- end }}
{{- end -}}

{{- define "aegis.egressPeer" -}}
- ipBlock:
    cidr: 0.0.0.0/0
    except:
      {{- toYaml . | nindent 6 }}
{{- end -}}
