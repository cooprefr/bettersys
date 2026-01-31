# Deploy probes to each region/instance combination
# Using for_each with provider aliases requires separate module blocks per region

# EU-WEST-1 (Ireland)
module "probe_eu_west_1" {
  for_each = { for k, v in local.active_instance_types : k => v if contains(keys(local.active_regions), "eu-west-1") }

  source = "./modules/probe"
  providers = {
    aws = aws.eu-west-1
  }

  region            = "eu-west-1"
  instance_type     = each.value.type
  instance_family   = each.value.family
  arch              = each.value.arch
  ssh_public_key    = var.ssh_public_key
  experiment_id     = local.experiment_id
  warmup_sec        = var.warmup_sec
  metrics_port      = var.metrics_port
  allowed_ssh_cidrs = var.allowed_ssh_cidrs
}

# EU-CENTRAL-1 (Frankfurt)
module "probe_eu_central_1" {
  for_each = { for k, v in local.active_instance_types : k => v if contains(keys(local.active_regions), "eu-central-1") }

  source = "./modules/probe"
  providers = {
    aws = aws.eu-central-1
  }

  region            = "eu-central-1"
  instance_type     = each.value.type
  instance_family   = each.value.family
  arch              = each.value.arch
  ssh_public_key    = var.ssh_public_key
  experiment_id     = local.experiment_id
  warmup_sec        = var.warmup_sec
  metrics_port      = var.metrics_port
  allowed_ssh_cidrs = var.allowed_ssh_cidrs
}

# EU-WEST-2 (London)
module "probe_eu_west_2" {
  for_each = { for k, v in local.active_instance_types : k => v if contains(keys(local.active_regions), "eu-west-2") }

  source = "./modules/probe"
  providers = {
    aws = aws.eu-west-2
  }

  region            = "eu-west-2"
  instance_type     = each.value.type
  instance_family   = each.value.family
  arch              = each.value.arch
  ssh_public_key    = var.ssh_public_key
  experiment_id     = local.experiment_id
  warmup_sec        = var.warmup_sec
  metrics_port      = var.metrics_port
  allowed_ssh_cidrs = var.allowed_ssh_cidrs
}

# AP-SOUTHEAST-1 (Singapore) - Phase 2
module "probe_ap_southeast_1" {
  for_each = { for k, v in local.active_instance_types : k => v if var.phase >= 2 && contains(keys(local.active_regions), "ap-southeast-1") }

  source = "./modules/probe"
  providers = {
    aws = aws.ap-southeast-1
  }

  region            = "ap-southeast-1"
  instance_type     = each.value.type
  instance_family   = each.value.family
  arch              = each.value.arch
  ssh_public_key    = var.ssh_public_key
  experiment_id     = local.experiment_id
  warmup_sec        = var.warmup_sec
  metrics_port      = var.metrics_port
  allowed_ssh_cidrs = var.allowed_ssh_cidrs
}

# ME-CENTRAL-1 (UAE) - Phase 2
module "probe_me_central_1" {
  for_each = { for k, v in local.active_instance_types : k => v if var.phase >= 2 && contains(keys(local.active_regions), "me-central-1") }

  source = "./modules/probe"
  providers = {
    aws = aws.me-central-1
  }

  region            = "me-central-1"
  instance_type     = each.value.type
  instance_family   = each.value.family
  arch              = each.value.arch
  ssh_public_key    = var.ssh_public_key
  experiment_id     = local.experiment_id
  warmup_sec        = var.warmup_sec
  metrics_port      = var.metrics_port
  allowed_ssh_cidrs = var.allowed_ssh_cidrs
}

# AP-NORTHEAST-1 (Tokyo) - Phase 2
module "probe_ap_northeast_1" {
  for_each = { for k, v in local.active_instance_types : k => v if var.phase >= 2 && contains(keys(local.active_regions), "ap-northeast-1") }

  source = "./modules/probe"
  providers = {
    aws = aws.ap-northeast-1
  }

  region            = "ap-northeast-1"
  instance_type     = each.value.type
  instance_family   = each.value.family
  arch              = each.value.arch
  ssh_public_key    = var.ssh_public_key
  experiment_id     = local.experiment_id
  warmup_sec        = var.warmup_sec
  metrics_port      = var.metrics_port
  allowed_ssh_cidrs = var.allowed_ssh_cidrs
}
