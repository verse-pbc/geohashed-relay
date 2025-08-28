# Geohashed Relay Scripts

This directory contains scripts for managing the geohashed relay deployment.

## Available Scripts

### `setup-sealed-secret.sh`

**Purpose**: Generates a Nostr private key and creates a Kubernetes sealed secret for persistent relay identity.

**Prerequisites**:
- `kubeseal` CLI tool installed
- `kubectl` configured with access to target cluster
- Sealed Secrets controller running in cluster

**Usage**:
```bash
./scripts/setup-sealed-secret.sh
```

**What it does**:
1. Checks prerequisites (kubeseal, kubectl, cluster access)
2. Creates `geohashed-relay` namespace if it doesn't exist
3. Generates a cryptographically secure 32-byte private key
4. Uses kubeseal to encrypt the private key for the target cluster
5. Updates `deployment/geohashed-relay/templates/sealed-secret.yaml` with the encrypted value
6. Provides verification steps and next actions

**Important Notes**:
- Save the generated private key securely - you'll need it for key rotation
- The script creates a backup of the original template file
- Never commit unencrypted private keys to the repository
- The encrypted value is cluster-specific and cannot be decrypted elsewhere

**Expected Output**:
```
🔐 Geohashed Relay Sealed Secret Setup
=================================================
✅ Prerequisites check passed
Generated private key: a1b2c3d4e5f6...
✅ Sealed secret created successfully
✅ Updated deployment/geohashed-relay/templates/sealed-secret.yaml

Next steps:
1. Commit the updated sealed-secret.yaml to git
2. Push to trigger the deployment pipeline
3. Verify deployment with kubectl logs...
```

## Development Workflow

1. **Initial Setup**: Run `setup-sealed-secret.sh` once per cluster/environment
2. **Deploy**: Commit and push changes to trigger CI/CD
3. **Verify**: Check logs for "Using provided relay private key" message
4. **Monitor**: Ensure public key remains consistent across pod restarts

## Troubleshooting

### "kubeseal not found"
Install the kubeseal CLI:
```bash
# macOS
brew install kubeseal

# Or download directly
wget https://github.com/bitnami-labs/sealed-secrets/releases/download/v0.24.0/kubeseal-0.24.0-darwin-amd64.tar.gz
tar -xzf kubeseal-0.24.0-darwin-amd64.tar.gz
sudo mv kubeseal /usr/local/bin/
```

### "sealed-secrets controller not found"
Install sealed-secrets in your cluster:
```bash
kubectl apply -f https://github.com/bitnami-labs/sealed-secrets/releases/download/v0.24.0/controller.yaml
```

### "Template file not found"
Make sure you're running the script from the project root directory where `deployment/geohashed-relay/` exists.

### Relay shows "Generating random keys"
This means the sealed secret isn't being loaded properly:
1. Check if the secret exists: `kubectl get secrets -n geohashed-relay`
2. Verify the secret is mounted: `kubectl describe pod -n geohashed-relay geohashed-relay-0`
3. Check for environment variable: `kubectl exec -n geohashed-relay geohashed-relay-0 -- env | grep RELAY_PRIVATE_KEY`

## Security Best Practices

- **Never commit unencrypted private keys** to version control
- **Back up private keys securely** in a password manager or vault
- **Rotate keys periodically** by re-running the setup script
- **Monitor logs** for any "Generating random keys" warnings
- **Use different keys** for different environments (dev/staging/prod)
- **Limit cluster access** to authorized personnel only

## Key Rotation

To rotate the relay's private key:
1. Run `setup-sealed-secret.sh` again to generate new key
2. Commit and push the updated sealed-secret.yaml
3. Restart the StatefulSet: `kubectl rollout restart statefulset/geohashed-relay -n geohashed-relay`
4. Verify the new public key appears in logs
5. Update your backup records with the new private key