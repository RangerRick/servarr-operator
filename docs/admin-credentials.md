# Admin Credentials

The operator can configure admin accounts for apps declaratively by referencing a Kubernetes
Secret containing the desired username and password. When the Secret is rotated, the operator
automatically propagates the new credentials without manual intervention.

---

## Secret Format

Create a Secret with `username` and `password` keys. The operator reads but never creates or
modifies this Secret.

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: media-admin
  namespace: servarr-system
stringData:
  username: admin
  password: changeme
```

---

## Configuring Credentials

Set `adminCredentials` on a `ServarrApp` or in the `defaults` / per-app block of a `MediaStack`.

### ServarrApp

```yaml
apiVersion: servarr.dev/v1alpha1
kind: ServarrApp
metadata:
  name: sonarr
spec:
  app: Sonarr
  adminCredentials:
    secretName: media-admin
```

### MediaStack — global defaults

```yaml
spec:
  defaults:
    adminCredentials:
      secretName: media-admin
  apps:
    sonarr: {}    # inherits from defaults
    radarr: {}    # inherits from defaults
```

### MediaStack — per-app override

```yaml
spec:
  defaults:
    adminCredentials:
      secretName: media-admin
  apps:
    sonarr: {}
    radarr:
      adminCredentials:
        secretName: radarr-admin   # overrides defaults for Radarr only
```

### Split-4K override

```yaml
spec:
  apps:
    sonarr:
      adminCredentials:
        secretName: media-admin
      split4k:
        adminCredentials:
          secretName: sonarr-4k-admin  # 4K instance uses a different secret
```

---

## How Credentials Are Applied

The mechanism varies by app type.

### Servarr v3 apps (Sonarr, Radarr, Lidarr, Prowlarr)

Credentials are injected as environment variables in the Deployment, using the double-underscore
override pattern native to these apps:

| Env var | Source |
|---------|--------|
| `{APP}__AUTH__USERNAME` | `secret.username` |
| `{APP}__AUTH__PASSWORD` | `secret.password` |
| `{APP}__AUTH__METHOD`   | `Forms` (hardcoded) |

When the Secret is rotated, the operator computes a SHA-256 checksum of the credentials and
writes it as a pod template annotation (`servarr.dev/admin-credentials-checksum`). The changed
annotation triggers a rolling update, causing pods to restart and pick up the new
`secretKeyRef` values.

### API-configured apps

For apps that expose credential management through their API, the operator makes a live API call
on each reconcile cycle. This means credentials are applied immediately after the app becomes
healthy, and re-applied on every reconcile.

| App | Mechanism |
|-----|-----------|
| SABnzbd | `GET /api?mode=set_config&section=misc&keyword=username/password` |
| Transmission | `session-set` RPC (`rpc-username`, `rpc-password`, `rpc-authentication-required`) |
| Jellyfin | Startup wizard (`POST /Startup/User`) on first run; `POST /Users/{id}/Password` thereafter |
| Tautulli | `POST /api/v2?cmd=set_credentials` |
| Overseerr | `PUT /api/v1/auth/local` |

### Unsupported apps

| App | Reason |
|-----|--------|
| Plex | Authentication is managed through your plex.tv account — no local admin credential API exists |
| Maintainerr | Uses Plex authentication; no separate credential API |

For these apps, `adminCredentials` is accepted at the CRD level but has no effect at runtime.

---

## Secret Rotation

Rotating credentials requires only updating the Secret:

```sh
kubectl patch secret media-admin -n servarr-system \
  --patch='{"stringData":{"password":"newpassword"}}'
```

- **Servarr v3 apps** restart automatically (rolling update triggered by checksum annotation change).
- **API-configured apps** pick up the new credentials on the next reconcile cycle (typically within seconds).

---

## Status

After credentials are successfully configured, the `AdminCredentialsConfigured` condition on the
`ServarrApp` is set to `True`:

```sh
kubectl get servarrapp sonarr -n servarr-system \
  -o jsonpath='{.status.conditions[?(@.type=="AdminCredentialsConfigured")]}'
```

---

## Security Notes

- The operator never logs credential values.
- The operator does not create, own, or delete the credentials Secret; lifecycle is entirely under
  user control.
- Credential values are transmitted to apps over their service-internal network path only.
