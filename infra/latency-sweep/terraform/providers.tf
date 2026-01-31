# Multi-region provider aliases
# Each region needs its own provider for cross-region deployment

provider "aws" {
  alias  = "eu-west-1"
  region = "eu-west-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "eu-central-1"
  region = "eu-central-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "eu-west-2"
  region = "eu-west-2"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "ap-southeast-1"
  region = "ap-southeast-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "me-central-1"
  region = "me-central-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}

provider "aws" {
  alias  = "ap-northeast-1"
  region = "ap-northeast-1"
  default_tags {
    tags = {
      Project    = "betterbot-latency-sweep"
      Experiment = local.experiment_id
      ManagedBy  = "terraform"
    }
  }
}
