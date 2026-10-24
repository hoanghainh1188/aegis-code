output "plan_role_arn" {
  description = "Biến repo GitHub `AWS_PLAN_ROLE_ARN`."
  value       = aws_iam_role.plan.arn
}

output "image_role_arn" {
  description = "Biến repo GitHub `AWS_IMAGE_ROLE_ARN`."
  value       = aws_iam_role.image.arn
}

output "apply_role_arn" {
  description = "Biến của từng GitHub environment `AWS_APPLY_ROLE_ARN`."
  value       = aws_iam_role.apply.arn
}

output "ecr_repositories" {
  description = "URL các repository ECR."
  value       = { for k, r in aws_ecr_repository.image : k => r.repository_url }
}
