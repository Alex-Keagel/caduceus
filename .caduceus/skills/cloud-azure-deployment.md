---
name: cloud-azure-deployment
version: "1.0"
description: Azure deployment patterns — Container Apps, Functions, App Service, Blob Storage, and Key Vault
categories: [cloud, azure, deployment, devops]
triggers: ["azure container apps deploy", "azure functions deploy", "azure app service", "azure blob storage setup", "azure key vault secrets"]
tools: [read_file, edit_file, shell, run_tests]
---

# Azure Deployment Patterns Skill

## Toolchain
```bash
az login
az account set --subscription MY_SUBSCRIPTION_ID
az group create --name my-rg --location eastus
```

## Azure Container Apps (Containers — Recommended)
```bash
az containerapp env create --name my-env --resource-group my-rg --location eastus

az containerapp create \
  --name my-app --resource-group my-rg --environment my-env \
  --image mcr.microsoft.com/azuredocs/containerapps-helloworld \
  --target-port 80 --ingress external \
  --min-replicas 0 --max-replicas 10 \
  --set-env-vars NODE_ENV=production
```
- Scale to zero with `--min-replicas 0` for cost savings on intermittent workloads
- Use Managed Identity for all Azure service access (Storage, Key Vault, Service Bus)

## Azure Functions (Serverless)
```bash
func init my-func --typescript
func new --name HttpTrigger --template "HTTP trigger"
func azure functionapp publish MY_FUNCTION_APP
```
- Set `WEBSITE_RUN_FROM_PACKAGE=1` for immutable, faster cold-start deployments
- Use Durable Functions for long-running orchestration workflows
- Apply KEDA-based scaling rules for queue-triggered functions

## App Service (Web Apps)
```bash
az webapp create --name my-app --resource-group my-rg \
  --plan my-plan --runtime "NODE:20-lts"
az webapp config appsettings set --name my-app --resource-group my-rg \
  --settings NODE_ENV=production
az webapp deploy --name my-app --resource-group my-rg --src-path ./dist.zip
```
Reference Key Vault secrets in settings: `@Microsoft.KeyVault(VaultName=my-vault;SecretName=DB-URL)`

## Blob Storage
```bash
az storage account create --name mystg --resource-group my-rg --sku Standard_LRS
az storage container create --name uploads --account-name mystg
az storage blob upload-batch --source ./files --destination uploads --account-name mystg
```
- Disable public blob access; use SAS tokens or Managed Identity for secure access
- Enable soft delete (7–30 day retention) and versioning on important containers

## Key Vault
```bash
az keyvault create --name my-vault --resource-group my-rg
az keyvault secret set --vault-name my-vault --name "DB-PASSWORD" --value "secret"
```
Grant Managed Identity access: `az keyvault set-policy --name my-vault --object-id MI_OBJECT_ID --secret-permissions get`

## CI/CD with GitHub Actions
```yaml
- uses: azure/login@v2
  with:
    creds: ${{ secrets.AZURE_CREDENTIALS }}
- uses: azure/webapps-deploy@v3
  with:
    app-name: my-app
    package: ./dist.zip
```
Use federated identity credentials (OIDC) instead of `AZURE_CREDENTIALS` secret for production pipelines.
