// Repo mẫu nhiều project cho profile Kotlin (#648): `repro_test` chạy
// `test --tests` ở project gốc, nên task test của mọi project đều chạy.
rootProject.name = "fixture-kotlin-multi"
include("cart", "tax")
