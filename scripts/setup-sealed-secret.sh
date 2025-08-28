#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}🔐 Geohashed Relay Sealed Secret Setup${NC}"
echo "================================================="

# Check prerequisites
echo -e "${BLUE}Checking prerequisites...${NC}"

if ! command -v kubeseal &> /dev/null; then
    echo -e "${RED}❌ kubeseal CLI not found. Please install it first:${NC}"
    echo "https://sealed-secrets.netlify.app/docs/overview/"
    exit 1
fi

if ! kubectl cluster-info &> /dev/null; then
    echo -e "${RED}❌ kubectl not configured or cluster not accessible${NC}"
    exit 1
fi

# Check if sealed-secrets controller is running
if ! kubectl get pods -n kube-system -l name=sealed-secrets-controller &> /dev/null; then
    echo -e "${YELLOW}⚠️  Warning: sealed-secrets controller not found in kube-system namespace${NC}"
    echo "Make sure sealed-secrets is installed in your cluster"
fi

echo -e "${GREEN}✅ Prerequisites check passed${NC}"
echo ""

# Check if namespace exists, create if it doesn't
echo -e "${BLUE}Checking namespace...${NC}"
if ! kubectl get namespace geohashed-relay &> /dev/null; then
    echo "Creating geohashed-relay namespace..."
    kubectl create namespace geohashed-relay
fi

# Generate private key
echo -e "${BLUE}Generating Nostr private key...${NC}"
PRIVATE_KEY=$(openssl rand -hex 32)
echo -e "${GREEN}Generated private key: ${PRIVATE_KEY}${NC}"
echo -e "${YELLOW}⚠️  IMPORTANT: Save this key securely! It will be needed for key rotation.${NC}"
echo ""

# Create sealed secret
echo -e "${BLUE}Creating sealed secret...${NC}"
SEALED_VALUE=$(echo -n "$PRIVATE_KEY" | kubeseal --raw \
  --from-file=/dev/stdin \
  --namespace geohashed-relay \
  --name geohashed-relay-secret \
  --scope namespace-wide)

if [ -z "$SEALED_VALUE" ]; then
    echo -e "${RED}❌ Failed to create sealed secret${NC}"
    exit 1
fi

echo -e "${GREEN}✅ Sealed secret created successfully${NC}"
echo ""

# Update the template
echo -e "${BLUE}Updating sealed secret template...${NC}"
TEMPLATE_FILE="deployment/geohashed-relay/templates/sealed-secret.yaml"

if [ ! -f "$TEMPLATE_FILE" ]; then
    echo -e "${RED}❌ Template file not found: $TEMPLATE_FILE${NC}"
    echo "Make sure you're running this from the project root directory"
    exit 1
fi

# Create backup
cp "$TEMPLATE_FILE" "$TEMPLATE_FILE.bak"

# Update the file
sed -i.tmp "s/REPLACE_WITH_SEALED_SECRET_VALUE/$SEALED_VALUE/" "$TEMPLATE_FILE"
rm "$TEMPLATE_FILE.tmp"

echo -e "${GREEN}✅ Updated $TEMPLATE_FILE${NC}"
echo ""

# Show what changed
echo -e "${BLUE}Template changes:${NC}"
echo "Before: relay-private-key: REPLACE_WITH_SEALED_SECRET_VALUE"
echo "After:  relay-private-key: ${SEALED_VALUE:0:20}... (truncated)"
echo ""

# Final instructions
echo -e "${GREEN}🎉 Setup complete!${NC}"
echo ""
echo -e "${BLUE}Next steps:${NC}"
echo "1. Commit the updated sealed-secret.yaml to git"
echo "2. Push to trigger the deployment pipeline"
echo "3. Verify deployment with:"
echo "   kubectl logs -n geohashed-relay geohashed-relay-0 | grep 'Relay public key'"
echo ""
echo -e "${YELLOW}Security reminders:${NC}"
echo "• Save the private key: ${PRIVATE_KEY}"
echo "• Never commit unencrypted keys to git"
echo "• The backup template is saved as: $TEMPLATE_FILE.bak"
echo ""
echo -e "${BLUE}To verify the relay identity after deployment:${NC}"
echo "kubectl logs -n geohashed-relay geohashed-relay-0 | grep 'Using provided relay private key'"