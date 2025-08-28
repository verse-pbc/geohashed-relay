# Sealed Secrets Setup for Geohashed Relay

The geohashed-relay requires a persistent Nostr private key to maintain a stable relay identity across restarts and deployments.

## Why This is Important

Without a persistent private key, the relay generates a new identity on each restart, which:
- Breaks client trust and NIP-11 relay information consistency
- Prevents proper relay identification in the Nostr network
- Makes monitoring and debugging more difficult

## Prerequisites

1. `kubeseal` CLI tool installed
2. Access to the target Kubernetes cluster with sealed-secrets controller
3. Cluster admin permissions for the `geohashed-relay` namespace

## Generate Relay Private Key

Choose one of these methods to generate a 32-byte hex private key:

### Method 1: Using OpenSSL (Recommended)
```bash
# Generate a cryptographically secure 32-byte hex string
openssl rand -hex 32
```

### Method 2: Using Nostr Tools
```bash
# If you have nostril installed
nostril --sec

# If you have nak installed  
nak key generate
```

### Method 3: Using the relay itself
```bash
# Run the relay locally to see what key it generates
RUST_LOG=info cargo run
# Copy the "Relay public key" from logs, then derive private key if needed
```

## Create Sealed Secret

1. Generate your private key (save it securely):
```bash
PRIVATE_KEY=$(openssl rand -hex 32)
echo "Generated private key: $PRIVATE_KEY"
echo "IMPORTANT: Save this key securely before proceeding!"
```

2. Create the sealed secret:
```bash
# Replace with your actual private key
echo -n "$PRIVATE_KEY" | kubeseal --raw \
  --from-file=/dev/stdin \
  --namespace geohashed-relay \
  --name geohashed-relay-secret \
  --scope namespace-wide

# The output will be a long encrypted string starting with "AgA..." or similar
# Copy this entire string for the next step
```

3. Update the template:
```bash
# Edit templates/sealed-secret.yaml and replace REPLACE_WITH_SEALED_SECRET_VALUE
# with the encrypted output from step 2
```

## Automated Setup Script

You can use this script to generate and seal the secret automatically:

```bash
#!/bin/bash
set -e

# Generate private key
PRIVATE_KEY=$(openssl rand -hex 32)
echo "Generated Nostr private key: $PRIVATE_KEY"

# Create sealed secret
SEALED_VALUE=$(echo -n "$PRIVATE_KEY" | kubeseal --raw \
  --from-file=/dev/stdin \
  --namespace geohashed-relay \
  --name geohashed-relay-secret \
  --scope namespace-wide)

echo "Sealed secret value: $SEALED_VALUE"

# Update the template
sed -i.bak "s/REPLACE_WITH_SEALED_SECRET_VALUE/$SEALED_VALUE/" \
  deployment/geohashed-relay/templates/sealed-secret.yaml

echo "✅ Updated templates/sealed-secret.yaml with sealed secret"
echo "⚠️  IMPORTANT: Save your private key securely: $PRIVATE_KEY"
echo "📝 The relay public key will be shown in logs when deployed"
```

## Verification Steps

### 1. Check Secret Creation
```bash
# Verify the sealed secret was applied
kubectl get sealedsecrets -n geohashed-relay
kubectl get secrets -n geohashed-relay geohashed-relay-secret
```

### 2. Check Pod Environment
```bash
# Verify the secret is mounted correctly
kubectl describe pod -n geohashed-relay geohashed-relay-0
```

### 3. Verify Relay Identity
```bash
# Check logs for consistent public key
kubectl logs -n geohashed-relay geohashed-relay-0 | grep "Relay public key"

# Restart pod and verify same public key
kubectl delete pod -n geohashed-relay geohashed-relay-0
sleep 30
kubectl logs -n geohashed-relay geohashed-relay-0 | grep "Relay public key"
```

### 4. Test Relay Info (NIP-11)
```bash
# Should return consistent relay information
curl -H "Accept: application/nostr+json" https://geohashed.verse.app/
```

## Production Checklist

- [ ] Private key generated securely (32 bytes hex)
- [ ] Sealed secret created with correct namespace scope
- [ ] Template updated with encrypted value
- [ ] Secret applied to cluster successfully
- [ ] Pod shows "Using provided relay private key" in logs
- [ ] Public key consistent across restarts
- [ ] Private key backed up securely
- [ ] Old placeholder removed from repository

## Key Rotation

To rotate the relay's private key:

1. Generate new private key
2. Create new sealed secret with same name
3. Apply to cluster (will update existing secret)
4. Restart the StatefulSet: `kubectl rollout restart statefulset/geohashed-relay -n geohashed-relay`
5. Verify new public key in logs

## Security Notes

- The private key is encrypted using the cluster's sealed secrets public key
- Only the sealed secrets controller in the target cluster can decrypt it
- Never commit unencrypted private keys to the repository
- Store backup of private key in secure password manager
- Monitor for any "Generating random keys" warnings in logs (indicates secret not loading)
- Consider key rotation schedule for enhanced security