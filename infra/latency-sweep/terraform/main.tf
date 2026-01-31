terraform {
  required_version = ">= 1.5"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }

  backend "s3" {
    bucket = "betterbot-terraform-state"
    key    = "latency-sweep/terraform.tfstate"
    region = "eu-west-1"
  }
}

locals {
  regions_phase1 = {
    "eu-west-1"    = { name = "Ireland" }
    "eu-central-1" = { name = "Frankfurt" }
    "eu-west-2"    = { name = "London" }
  }

  regions_phase2 = {
    "ap-southeast-1" = { name = "Singapore" }
    "me-central-1"   = { name = "UAE" }
    "ap-northeast-1" = { name = "Tokyo" }
  }

  active_regions = var.phase == 1 ? local.regions_phase1 : merge(local.regions_phase1, local.regions_phase2)

  instance_types = {
    c6i_large   = { type = "c6i.large", family = "compute", arch = "x86_64" }
    c6in_large  = { type = "c6in.large", family = "network", arch = "x86_64" }
    c7gn_medium = { type = "c7gn.medium", family = "network_arm", arch = "arm64" }
  }

  active_instance_types = { for k, v in local.instance_types : k => v if contains(var.instance_types_filter, k) }

  experiment_id = var.experiment_id != "" ? var.experiment_id : "latency-sweep-${formatdate("YYYYMMDD-hhmm", timestamp())}"

  probe_combinations = flatten([
    for region, region_cfg in local.active_regions : [
      for inst_key, inst_cfg in local.active_instance_types : {
        region          = region
        region_name     = region_cfg.name
        instance_key    = inst_key
        instance_type   = inst_cfg.type
        instance_family = inst_cfg.family
        arch            = inst_cfg.arch
      }
    ]
  ])
}
