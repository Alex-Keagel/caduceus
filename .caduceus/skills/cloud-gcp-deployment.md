---
name: cloud-gcp-deployment
version: "1.0"
description: GCP deployment patterns — Cloud Run, Artifact Registry, GCS, Cloud SQL, and Workload Identity
categories: [cloud, gcp, deployment, devops]
triggers: ["google cloud run deploy", "gcp cloud build", "gcs static site", "cloud sql connect", "gcp workload identity federation"]
tools: [read_file, edit_file, shell, run_tests]
---

# GCP Deployment Patterns Skill

## Toolchain
```bash
gcloud auth login
gcloud config set project MY_PROJECT_ID
gcloud services enable run.googleapis.com artifactregistry.googleapis.com cloudbuild.googleapis.com
```

## Cloud Run (Serverless Containers)
```bash
# Create Artifact Registry repo
gcloud artifacts repositories create my-repo \
  --repository-format=docker --location=us-central1

# Build and push via Cloud Build
gcloud builds submit --tag us-central1-docker.pkg.dev/PROJECT/my-repo/my-app:latest

# Deploy
gcloud run deploy my-app \
  --image us-central1-docker.pkg.dev/PROJECT/my-repo/my-app:latest \
  --region us-central1 --allow-unauthenticated \
  --set-env-vars NODE_ENV=production \
  --memory 512Mi --concurrency 80 \
  --min-instances 0 --max-instances 10
```
- Use `--no-allow-unauthenticated` for internal services; invoke via service account auth
- Mount secrets: `--set-secrets DB_PASSWORD=my-secret:latest`
- Set `--cpu-throttling` to reduce cold-start costs for low-traffic services

## GCS (Object Storage)
```bash
gsutil mb -l us-central1 gs://my-bucket
gsutil -m cp -r ./dist gs://my-bucket/
gsutil web set -m index.html -e 404.html gs://my-bucket   # static site
```
- Apply uniform bucket-level access; disable legacy ACLs
- Use signed URLs for time-limited private access
- Enable object versioning on data buckets for accidental-deletion protection

## Cloud SQL
- Create instance in same region as Cloud Run; use Cloud SQL Auth Proxy for local dev
- Connect from Cloud Run: `--set-cloudsql-instances PROJECT:REGION:INSTANCE`
- Use private IP with Serverless VPC Access connector in production for lower latency

## IAM
- Use Workload Identity Federation for CI/CD — eliminates service account key management
- Assign predefined roles at the resource level, not the project level
- Route Cloud Audit Logs to BigQuery for long-term compliance analysis

## CI/CD with GitHub Actions (Workload Identity Federation)
```yaml
- uses: google-github-actions/auth@v2
  with:
    workload_identity_provider: projects/NUMBER/locations/global/workloadIdentityPools/my-pool/providers/github
    service_account: deploy@PROJECT.iam.gserviceaccount.com
- uses: google-github-actions/deploy-cloudrun@v2
  with:
    service: my-app
    image: us-central1-docker.pkg.dev/PROJECT/my-repo/my-app:latest
    region: us-central1
```
