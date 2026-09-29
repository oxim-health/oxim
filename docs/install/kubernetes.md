# Running OXIM on Kubernetes

The Helm chart in [`deploy/helm/oxim`](../../deploy/helm/oxim) runs OXIM as a StatefulSet with one pod and a persistent volume for the SQLite store.

OXIM with SQLite is a single-node engine: the chart refuses `replicaCount` values other than 1. Clustering and high availability need the PostgreSQL backend. Kubernetes restarts a failed pod; queued messages survive on the volume.

## Install

```sh
helm install oxim deploy/helm/oxim --namespace oxim --create-namespace -f my-values.yaml
kubectl -n oxim logs -f oxim-0
```

A minimal `my-values.yaml` for an analyzer that sends ASTM over TCP:

```yaml
service:
  ports:
    - name: astm
      port: 5100
probes:
  port: astm
channels:
  analyzer.yaml: |
    id: analyzer-to-lis
    source:
      type: astm-tcp
      data_type: astm
      normalize: true
      settings: {listen: 0.0.0.0:5100}
    transformers:
      - {type: map-observations, table: analyzer-codes.csv}
    destinations:
      - id: lis
        type: mllp
        encoder: {type: hl7v2-oru-r01, sending_application: OXIM}
        settings: {target: lis.hospital.internal:2576}
tables:
  analyzer-codes.csv: |
    from,to,display
    GLU,1520,Glucose
```

## Values

| Value | Default | Meaning |
|---|---|---|
| `image.repository`, `image.tag` | `ghcr.io/oxim-health/oxim`, chart appVersion | image |
| `config` | log, retention and reload settings | `oxim.yaml` without the directories, which the chart sets; a change restarts the pod |
| `channels` | one MLLP archive channel | channel files by file name; changes apply without a restart |
| `tables` | none | code and routing tables by file name |
| `secret.create`, `secret.stringData`, `secret.existingSecret` | off | credentials mounted read-only at `/etc/oxim/secrets` |
| `service.type`, `service.ports` | ClusterIP, `mllp` 2575 | one entry per channel listener (`name`, `port`, optional `targetPort`, `protocol`); names have at most 15 characters |
| `service.externalTrafficPolicy` | empty | `Local` keeps client addresses for LoadBalancer services |
| `probes.port` | `mllp` | listener port name used for TCP startup, liveness and readiness probes; empty disables them |
| `persistence.*` | enabled, 10Gi, ReadWriteOnce | data volume; without it messages are lost with the pod |
| `resources` | 100m CPU and 128Mi requested, 512Mi limit | container resources |
| `terminationGracePeriodSeconds` | 75 | time for in-flight deliveries on shutdown |
| `podSecurityContext`, `securityContext` | non-root 10001, read-only root filesystem, no capabilities, RuntimeDefault seccomp | security settings |
| `extraEnv`, `extraVolumes`, `extraVolumeMounts` | none | additions, for example certificates |
| `nodeSelector`, `tolerations`, `affinity` | none | scheduling |

Analyzers and LIS systems usually connect to a fixed address: expose the listeners with a `LoadBalancer` or `NodePort` service, or reach OXIM through the cluster network.

## Operations

```sh
kubectl -n oxim exec oxim-0 -- oxim -c /etc/oxim/oxim.yaml validate
kubectl -n oxim exec oxim-0 -- oxim -c /etc/oxim/oxim.yaml messages list --status error
```

Back up the volume `data-oxim-0` with your volume snapshot tooling. Upgrades are `helm upgrade` with the new chart and image; the pod restarts and the store is migrated automatically.
