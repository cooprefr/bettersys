variable "phase" {
  type        = number
  default     = 1
  description = "Deployment phase: 1 = EU regions only, 2 = EU + Asia/ME"

  validation {
    condition     = var.phase >= 1 && var.phase <= 2
    error_message = "Phase must be 1 or 2."
  }
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key for probe instances"
}

variable "instance_types_filter" {
  type        = list(string)
  default     = ["c6i_large", "c6in_large"]
  description = "Instance types to deploy (subset of: c6i_large, c6in_large, c7gn_medium)"
}

variable "experiment_id" {
  type        = string
  default     = ""
  description = "Experiment ID for tagging (auto-generated if empty)"
}

variable "warmup_sec" {
  type        = number
  default     = 300
  description = "Warmup duration in seconds"
}

variable "metrics_port" {
  type        = number
  default     = 9090
  description = "Port for metrics HTTP endpoint"
}

variable "allowed_ssh_cidrs" {
  type        = list(string)
  default     = ["0.0.0.0/0"]
  description = "CIDR blocks allowed to SSH to probes"
}
