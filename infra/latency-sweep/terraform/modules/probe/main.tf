variable "region" {
  type = string
}

variable "instance_type" {
  type = string
}

variable "instance_family" {
  type = string
}

variable "arch" {
  type    = string
  default = "x86_64"
}

variable "ssh_public_key" {
  type = string
}

variable "experiment_id" {
  type = string
}

variable "warmup_sec" {
  type    = number
  default = 300
}

variable "metrics_port" {
  type    = number
  default = 9090
}

variable "allowed_ssh_cidrs" {
  type    = list(string)
  default = ["0.0.0.0/0"]
}

data "aws_ami" "amazon_linux_2023" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["al2023-ami-*-kernel-*"]
  }

  filter {
    name   = "architecture"
    values = [var.arch]
  }

  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }
}

resource "aws_key_pair" "probe" {
  key_name   = "latency-probe-${var.region}-${var.instance_family}"
  public_key = var.ssh_public_key
}

resource "aws_security_group" "probe" {
  name_prefix = "latency-probe-${var.instance_family}-"
  description = "Latency probe - Binance WS egress + SSH/metrics ingress"

  ingress {
    description = "SSH"
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = var.allowed_ssh_cidrs
  }

  ingress {
    description = "Metrics endpoint"
    from_port   = var.metrics_port
    to_port     = var.metrics_port
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    description = "All egress"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name = "latency-probe-${var.region}-${var.instance_family}"
  }

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_iam_role" "probe" {
  name_prefix = "latency-probe-${var.instance_family}-"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action = "sts:AssumeRole"
      Effect = "Allow"
      Principal = {
        Service = "ec2.amazonaws.com"
      }
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.probe.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy_attachment" "cloudwatch" {
  role       = aws_iam_role.probe.name
  policy_arn = "arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy"
}

resource "aws_iam_instance_profile" "probe" {
  name_prefix = "latency-probe-${var.instance_family}-"
  role        = aws_iam_role.probe.name
}

resource "aws_instance" "probe" {
  ami                  = data.aws_ami.amazon_linux_2023.id
  instance_type        = var.instance_type
  key_name             = aws_key_pair.probe.key_name
  iam_instance_profile = aws_iam_instance_profile.probe.name

  vpc_security_group_ids = [aws_security_group.probe.id]

  ebs_optimized     = true
  source_dest_check = false

  root_block_device {
    volume_type = "gp3"
    volume_size = 20
    iops        = 3000
    throughput  = 125
  }

  user_data = base64encode(templatefile("${path.module}/../../bootstrap/install.sh.tpl", {
    experiment_id   = var.experiment_id
    region          = var.region
    instance_family = var.instance_family
    warmup_sec      = var.warmup_sec
    metrics_port    = var.metrics_port
    arch            = var.arch
  }))

  tags = {
    Name           = "latency-probe-${var.region}-${var.instance_family}"
    Region         = var.region
    InstanceFamily = var.instance_family
    ExperimentId   = var.experiment_id
  }

  lifecycle {
    create_before_destroy = true
  }
}

output "instance_id" {
  value = aws_instance.probe.id
}

output "public_ip" {
  value = aws_instance.probe.public_ip
}

output "private_ip" {
  value = aws_instance.probe.private_ip
}

output "region" {
  value = var.region
}

output "instance_family" {
  value = var.instance_family
}
