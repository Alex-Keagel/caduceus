---
name: cloud-aws-deployment
version: "1.0"
description: AWS deployment patterns for Lambda, ECS, S3, RDS with IAM best practices and CI/CD via GitHub Actions
categories: [cloud, aws, deployment, devops]
triggers: ["aws lambda deploy", "ecs fargate deploy", "aws s3 static site", "aws rds database", "github actions aws oidc"]
tools: [read_file, edit_file, shell, run_tests]
---

# AWS Deployment Patterns Skill

## Toolchain
```bash
pip install awscli
npm install -g aws-cdk
aws configure     # access key, secret key, default region
```

## Lambda (Serverless Functions)
```python
# handler.py
def handler(event, context):
    return {"statusCode": 200, "body": "Hello"}
```
```bash
zip function.zip handler.py
aws lambda create-function --function-name my-fn \
  --runtime python3.12 --role arn:aws:iam::ACCOUNT:role/lambda-role \
  --handler handler.handler --zip-file fileb://function.zip
aws lambda update-function-code --function-name my-fn --zip-file fileb://function.zip
```
- Use Lambda Layers for shared dependencies across multiple functions
- Use AWS Lambda Powertools for structured logging, X-Ray tracing, and custom metrics
- Set `ReservedConcurrentExecutions` to protect downstream services from overload

## ECS Fargate (Containers)
```bash
aws ecr get-login-password | docker login --username AWS --password-stdin ACCOUNT.dkr.ecr.REGION.amazonaws.com
docker build -t my-app . && docker push ACCOUNT.dkr.ecr.REGION.amazonaws.com/my-app:latest
```
1. Define Task Definition: CPU/memory, container image URI, port mappings, env vars from Secrets Manager
2. Create Service with target group, ALB listener, and auto-scaling policy based on CPU/memory

## S3 (Object Storage)
```bash
aws s3 mb s3://my-bucket --region us-east-1
aws s3 sync ./dist s3://my-bucket --delete
```
- Block all public access by default; serve public sites via CloudFront + OAC
- Enable versioning and lifecycle rules (e.g., transition to Glacier after 90 days)
- Use server-side encryption (SSE-S3 or SSE-KMS) for data at rest

## RDS (Managed Database)
- Deploy in private subnets; access from ECS tasks via security group rules
- Use SSM Session Manager port forwarding for developer access — no SSH keys
- Enable Multi-AZ and automated backups (7–35 day retention window) for production
- Use parameter groups for tuning; never SSH directly into RDS instances

## IAM Best Practices
- Apply least privilege: no `*` on sensitive actions or resources
- Use IAM roles (execution roles, instance profiles) — never embed long-lived access keys
- Enable CloudTrail in all regions for API audit logging
- Use SCPs in AWS Organizations to enforce guardrails across accounts

## CI/CD with GitHub Actions (OIDC)
```yaml
- uses: aws-actions/configure-aws-credentials@v4
  with:
    role-to-assume: arn:aws:iam::ACCOUNT:role/github-deploy
    aws-region: us-east-1
- run: aws ecs update-service --cluster prod --service my-svc --force-new-deployment
```
Use OIDC `role-to-assume` — do not store AWS credentials as long-lived secrets.
