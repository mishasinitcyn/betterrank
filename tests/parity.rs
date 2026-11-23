use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_betterrank"))
}

fn temp_dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("betterrank-rs-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn multiroot_ts_workspace_and_parse_cache_reuse() {
    let base = temp_dir("multiroot-ts");
    let cache_dir = base.join("cache");
    let pkg = base.join("packages/shared-intake");
    let app = base.join("apps/workspace");

    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::create_dir_all(app.join("src/server")).unwrap();

    fs::write(
        pkg.join("package.json"),
        r#"{"name":"@demo/shared-intake","main":"src/index.ts","exports":{".":"./src/index.ts","./*":"./src/*.ts"}}"#,
    )
    .unwrap();
    fs::write(
        pkg.join("src/index.ts"),
        "export { submitAccount } from './submit-account';\n",
    )
    .unwrap();
    fs::write(
        pkg.join("src/submit-account.ts"),
        "export function submitAccount(id: string) {\n  return id.trim();\n}\n",
    )
    .unwrap();
    fs::write(app.join("package.json"), r#"{"name":"@demo/workspace"}"#).unwrap();
    fs::write(
        app.join("src/server/handler.ts"),
        "import { submitAccount } from '@demo/shared-intake';\nexport function run(id: string) {\n  return submitAccount(id);\n}\n",
    )
    .unwrap();

    // Warm single roots first
    for r in [&pkg, &app] {
        let out = Command::new(bin_path())
            .env("CODE_INDEX_CACHE_DIR", &cache_dir)
            .args(["stats", "--root", r.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    // Compose multi-root: should reuse warm parse caches and resolve @demo/shared-intake
    let out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "callers",
            "submitAccount",
            "--no-noncode",
            "--root",
            pkg.to_str().unwrap(),
            "--root",
            app.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("workspace/src/server/handler.ts"),
        "expected cross-root caller, got:\n{stdout}"
    );
    assert!(
        stderr.contains("3 cached"),
        "expected warm parse cache reuse, got stderr:\n{stderr}"
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn auto_noncode_mentions_and_bounded_ranked_grep() {
    let base = temp_dir("noncode");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("pkg")).unwrap();
    fs::create_dir_all(base.join(".github/workflows")).unwrap();

    fs::write(
        base.join("pkg/check_policy.py"),
        "ENABLED_CHECKS = ('a', 'b')\n\ndef validate_policy(x):\n    return set(x) - set(ENABLED_CHECKS)\n",
    )
    .unwrap();
    fs::write(
        base.join("pkg/consumer.py"),
        "from pkg.check_policy import validate_policy\n\ndef run():\n    return validate_policy(['a'])\n",
    )
    .unwrap();
    fs::write(
        base.join("worker.Dockerfile"),
        "FROM python:3.11\nRUN python3 -c \"from pkg.check_policy import validate_policy\"\n",
    )
    .unwrap();
    fs::write(
        base.join(".github/workflows/ci.yml"),
        "name: ci\njobs:\n  test:\n    steps:\n      - run: python -c 'import check_policy'\n",
    )
    .unwrap();
    fs::write(
        base.join("schema.prisma"),
        "model Item {\n  id String @id\n  @@index([id])\n}\n",
    )
    .unwrap();

    // `dependents` automatically includes Dockerfile and CI YAML mentions!
    let dep_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["dependents", "pkg/check_policy.py", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(dep_out.status.success());
    let dep_stdout = String::from_utf8_lossy(&dep_out.stdout);
    assert!(dep_stdout.contains("pkg/consumer.py"));
    assert!(
        dep_stdout.contains("worker.Dockerfile"),
        "expected Dockerfile in auto non-code mentions:\n{dep_stdout}"
    );
    assert!(
        dep_stdout.contains(".github/workflows/ci.yml"),
        "expected CI workflow in auto non-code mentions:\n{dep_stdout}"
    );

    // `callers` on same-file Python constant
    let const_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["callers", "ENABLED_CHECKS", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&const_out.stdout).contains("pkg/check_policy.py"));

    // `grep --non-code` finds Prisma schema cleanly
    let grep_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "@@index",
            "--non-code",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(grep_out.status.success());
    assert!(String::from_utf8_lossy(&grep_out.stdout).contains("schema.prisma"));

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn outline_depth_lines_and_b2_b4_precision() {
    let base = temp_dir("b2-b4");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("src/routers")).unwrap();
    fs::create_dir_all(base.join("src/lib")).unwrap();

    fs::write(
        base.join("src/routers/review.ts"),
        "export const reviewRouter = createTRPCRouter({\n  getItem: publicProcedure\n    .query(() => 'review'),\n  reject: publicProcedure\n    .mutation(() => true),\n});\n",
    )
    .unwrap();
    fs::write(
        base.join("src/routers/disputes.ts"),
        "export const disputesRouter = createTRPCRouter({\n  getItem: publicProcedure\n    .query(() => 'dispute'),\n  update: publicProcedure\n    .mutation(() => true),\n  reasonCodes: publicProcedure\n    .query(() => ['a']),\n});\n",
    )
    .unwrap();
    fs::write(
        base.join("src/routers/disputes.test.ts"),
        "import { disputesRouter } from './disputes';\nimport { reviewRouter } from './review';\nconst caller = createCallerFactory(disputesRouter)({});\nconst liveCaller = createCallerFactory(reviewRouter)({});\nexport async function t() {\n  await caller.getItem();\n  await liveCaller.reject();\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("src/lib/worker.ts"),
        "import { createHash } from 'node:crypto';\nexport function summarize(doc: { reasonCodes: string[] }) {\n  const h = createHash('sha256');\n  h.update(doc.reasonCodes.join(','));\n  return h.digest('hex');\n}\n",
    )
    .unwrap();

    let rev_callers = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "callers",
            "getItem",
            "--file",
            "src/routers/review.ts",
            "--no-noncode",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let rev_stdout = String::from_utf8_lossy(&rev_callers.stdout);
    assert!(
        !rev_stdout.contains("disputes.test.ts"),
        "disputes.test.ts must not leak into review.ts::getItem:\n{rev_stdout}"
    );

    let callees = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "callees",
            "summarize",
            "--file",
            "src/lib/worker.ts",
            "--depth",
            "1",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let cal_stdout = String::from_utf8_lossy(&callees.stdout);
    assert!(!cal_stdout.contains("reasonCodes") && !cal_stdout.contains("update"));

    // Outline --depth 2 --lines folding
    let mut big = String::from("export function bigFn() {\n");
    for i in 0..40 {
        big.push_str(&format!("  const x{i} = {i};\n"));
    }
    big.push_str("  return 1;\n}\n");
    fs::write(base.join("src/lib/big.ts"), &big).unwrap();

    let folded = Command::new(bin_path())
        .args([
            "outline",
            "src/lib/big.ts",
            "--depth",
            "2",
            "--lines",
            "1-42",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fold_stdout = String::from_utf8_lossy(&folded.stdout);
    assert!(!fold_stdout.contains("const x20 = 20;"));
    assert!(fold_stdout.lines().count() < 10);

    let _ = fs::remove_dir_all(&base);
}
