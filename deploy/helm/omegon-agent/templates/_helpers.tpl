{{- define "omegon-agent.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "omegon-agent.fullname" -}}
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

{{- define "omegon-agent.labels" -}}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: {{ include "omegon-agent.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
styrene.sh/agent-id: {{ .Values.agent.id }}
{{- end }}

{{- define "omegon-agent.selectorLabels" -}}
app.kubernetes.io/name: {{ include "omegon-agent.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "omegon-agent.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "omegon-agent.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* Pod spec shared between Deployment and CronJob */}}
{{- define "omegon-agent.podSpec" -}}
serviceAccountName: {{ include "omegon-agent.serviceAccountName" . }}
{{- with .Values.imagePullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
containers:
  - name: agent
    image: "{{ .Values.image.repository }}:{{ .Values.image.tag }}"
    imagePullPolicy: {{ .Values.image.pullPolicy }}
    args:
      - "--agent"
      - {{ .Values.agent.id | quote }}
      - "--model"
      - {{ .Values.agent.model | quote }}
      {{- range .Values.agent.extraArgs }}
      - {{ . | quote }}
      {{- end }}
    ports:
      - containerPort: 7842
        protocol: TCP
    envFrom:
      {{- if and (eq .Values.secrets.provider "env") .Values.secrets.secretName }}
      - secretRef:
          name: {{ .Values.secrets.secretName }}
      {{- end }}
    env:
      - name: VOX_CONFIG
        value: /root/.config/vox/vox.toml
    volumeMounts:
      - name: vox-config
        mountPath: /root/.config/vox/vox.toml
        subPath: vox.toml
      {{- if .Values.secrets.authJson.enabled }}
      - name: auth-json
        mountPath: /config/omegon
        readOnly: true
      {{- end }}
    resources:
      {{- toYaml .Values.resources | nindent 6 }}
volumes:
  - name: vox-config
    configMap:
      name: {{ include "omegon-agent.fullname" . }}-vox
  {{- if .Values.secrets.authJson.enabled }}
  - name: auth-json
    secret:
      secretName: {{ .Values.secrets.authJson.secretName }}
      items:
        - key: {{ .Values.secrets.authJson.key }}
          path: auth.json
  {{- end }}
{{- with .Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end }}
