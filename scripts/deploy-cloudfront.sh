#!/bin/bash
set -e

echo "🚀 Deploying CloudFront for API Gateway image caching..."

# Deploy CloudFormation stack
aws cloudformation deploy \
  --template-file cloudfront-api-cache.yaml \
  --stack-name doxle-cloudfront-api-cache \
  --parameter-overrides \
    ApiGatewayDomain="api.doxle.ai" \
  --capabilities CAPABILITY_IAM \
  --region us-east-1

echo ""
echo "✅ CloudFront deployed successfully!"
echo ""
echo "Getting CloudFront domain..."
CLOUDFRONT_DOMAIN=$(aws cloudformation describe-stacks \
  --stack-name doxle-cloudfront-api-cache \
  --region us-east-1 \
  --query 'Stacks[0].Outputs[?OutputKey==`CloudFrontDomain`].OutputValue' \
  --output text)

DISTRIBUTION_ID=$(aws cloudformation describe-stacks \
  --stack-name doxle-cloudfront-api-cache \
  --region us-east-1 \
  --query 'Stacks[0].Outputs[?OutputKey==`CloudFrontDistributionId`].OutputValue' \
  --output text)

echo ""
echo "📦 CloudFront Distribution"
echo "  Domain: https://${CLOUDFRONT_DOMAIN}"
echo "  Distribution ID: ${DISTRIBUTION_ID}"
echo ""
echo "🔧 Next steps:"
echo "  1. Update frontend to use: https://${CLOUDFRONT_DOMAIN}/proxy-image/..."
echo "  2. Wait 5-10 minutes for distribution to deploy globally"
echo "  3. Test: curl https://${CLOUDFRONT_DOMAIN}/proxy-image/projects/YOUR_PROJECT/blocks/YOUR_BLOCK/image.jpg"
echo ""
echo "💡 To invalidate cache (if needed):"
echo "  aws cloudfront create-invalidation --distribution-id ${DISTRIBUTION_ID} --paths '/proxy-image/*'"
