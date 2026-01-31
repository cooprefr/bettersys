output "experiment_id" {
  value       = local.experiment_id
  description = "Experiment identifier for this deployment"
}

output "probe_endpoints" {
  value = merge(
    { for k, v in module.probe_eu_west_1 : "eu-west-1/${k}" => {
      region          = v.region
      instance_family = v.instance_family
      public_ip       = v.public_ip
      instance_id     = v.instance_id
      metrics_url     = "http://${v.public_ip}:${var.metrics_port}/metrics"
    } },
    { for k, v in module.probe_eu_central_1 : "eu-central-1/${k}" => {
      region          = v.region
      instance_family = v.instance_family
      public_ip       = v.public_ip
      instance_id     = v.instance_id
      metrics_url     = "http://${v.public_ip}:${var.metrics_port}/metrics"
    } },
    { for k, v in module.probe_eu_west_2 : "eu-west-2/${k}" => {
      region          = v.region
      instance_family = v.instance_family
      public_ip       = v.public_ip
      instance_id     = v.instance_id
      metrics_url     = "http://${v.public_ip}:${var.metrics_port}/metrics"
    } },
    var.phase >= 2 ? { for k, v in module.probe_ap_southeast_1 : "ap-southeast-1/${k}" => {
      region          = v.region
      instance_family = v.instance_family
      public_ip       = v.public_ip
      instance_id     = v.instance_id
      metrics_url     = "http://${v.public_ip}:${var.metrics_port}/metrics"
    } } : {},
    var.phase >= 2 ? { for k, v in module.probe_me_central_1 : "me-central-1/${k}" => {
      region          = v.region
      instance_family = v.instance_family
      public_ip       = v.public_ip
      instance_id     = v.instance_id
      metrics_url     = "http://${v.public_ip}:${var.metrics_port}/metrics"
    } } : {},
    var.phase >= 2 ? { for k, v in module.probe_ap_northeast_1 : "ap-northeast-1/${k}" => {
      region          = v.region
      instance_family = v.instance_family
      public_ip       = v.public_ip
      instance_id     = v.instance_id
      metrics_url     = "http://${v.public_ip}:${var.metrics_port}/metrics"
    } } : {}
  )
  description = "Map of probe endpoints with connection details"
}

output "phase" {
  value       = var.phase
  description = "Current deployment phase"
}

output "active_regions" {
  value       = keys(local.active_regions)
  description = "Regions included in this deployment"
}

output "active_instance_types" {
  value       = [for k, v in local.active_instance_types : v.type]
  description = "Instance types included in this deployment"
}
