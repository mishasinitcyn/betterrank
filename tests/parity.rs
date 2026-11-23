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

#[test]
fn global_flags_before_subcommand_and_multiword_search() {
    let base = temp_dir("cli-flags-search");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("src/account")).unwrap();
    fs::write(
        base.join("src/account/member-settings.ts"),
        "export function listAccountMembers() { return []; }\nexport const ACCOUNT_SETTINGS_ROLES = ['admin'];\n",
    )
    .unwrap();

    // Global `--root` BEFORE subcommand
    let out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["--root", base.to_str().unwrap(), "map", "--limit", "10"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let map_out = String::from_utf8_lossy(&out.stdout);
    assert!(map_out.contains("listAccountMembers"), "got:\n{map_out}");

    // `--root=<path>` equals form + multi-word `search`
    let search_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            &format!("--root={}", base.to_str().unwrap()),
            "search",
            "account settings roles",
            "--no-noncode",
        ])
        .output()
        .unwrap();
    assert!(search_out.status.success());
    let s = String::from_utf8_lossy(&search_out.stdout);
    assert!(
        s.contains("ACCOUNT_SETTINGS_ROLES"),
        "expected multi-word tokenized search match, got:\n{s}"
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn grep_enclosing_symbol_context_and_batch_callers() {
    let base = temp_dir("grep-sym-batch-callers");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(
        base.join("src/helpers.ts"),
        "export function alphaHelper(x: number) {\n  return x + 1;\n}\nexport function betaHelper(y: number) {\n  return y * 2;\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("src/consumer.ts"),
        "import { alphaHelper, betaHelper } from './helpers';\n\nexport function runPipeline(v: number) {\n  const step1 = alphaHelper(v);\n  const magicToken = 'SENTINEL_MARKER';\n  return betaHelper(step1) + magicToken.length;\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("Dockerfile"),
        "FROM node:22\nENV MARKER=SENTINEL_MARKER\n",
    )
    .unwrap();

    // Warm index so enclosing symbol & cached noncode_files are populated in .brbin
    let warm = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(warm.status.success());

    // `grep` with `-C 1` shows enclosing symbol `[runPipeline]` AND surrounding context lines
    let grep_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "SENTINEL_MARKER", "-C", "1", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(grep_out.status.success());
    let gs = String::from_utf8_lossy(&grep_out.stdout);
    assert!(gs.contains("[in runPipeline 3-7]│"), "expected enclosing symbol + range, got:\n{gs}");
    assert!(gs.contains("4-   const step1 = alphaHelper(v);"), "expected before context, got:\n{gs}");
    assert!(gs.contains("6-   return betaHelper(step1)"), "expected after context, got:\n{gs}");
    assert!(gs.contains("Dockerfile:"), "expected Dockerfile hit, got:\n{gs}");

    // `grep --non-code` uses cached noncode_files from .brbin
    let nc_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "SENTINEL_MARKER", "--non-code", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(nc_out.status.success());
    let ncs = String::from_utf8_lossy(&nc_out.stdout);
    assert!(ncs.contains("Dockerfile:") && !ncs.contains("consumer.ts"), "got:\n{ncs}");

    // Comma-separated `callers alphaHelper,betaHelper` batches both in one invocation
    let callers_out = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["callers", "alphaHelper,betaHelper", "--no-noncode", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(callers_out.status.success());
    let cs = String::from_utf8_lossy(&callers_out.stdout);
    assert!(cs.contains("=== callers: alphaHelper ==="), "got:\n{cs}");
    assert!(cs.contains("=== callers: betaHelper ==="), "got:\n{cs}");
    assert!(cs.contains("src/consumer.ts"), "got:\n{cs}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn ux_audit_all_five_fixes() {
    let base = temp_dir("ux-audit-v050");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("src/team-settings")).unwrap();
    fs::create_dir_all(base.join("src/__tests__")).unwrap();
    fs::create_dir_all(base.join(".github/workflows")).unwrap();

    fs::write(
        base.join("src/team-settings/shared.ts"),
        "export function getItem(id: string) {\n  // internal comment\n  return id;\n}\nexport function helperB() { return 2; }\n",
    )
    .unwrap();
    fs::write(
        base.join("src/other-router.ts"),
        "export function getItem(id: string) {\n  return id + '!';\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("src/team-settings/page.tsx"),
        "import { getItem, helperB } from './shared';\n\nexport function DashboardView() {\n  const a = getItem('1');\n  const b = getItem('2');\n  return a + b + helperB();\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("src/__tests__/dashboard.test.ts"),
        "import { getItem } from '../team-settings/shared';\nexport function testIt() { return getItem('test'); }\n",
    )
    .unwrap();
    fs::write(
        base.join(".github/workflows/ci.yml"),
        "name: ci\n# Uses shared cache across jobs\njobs:\n  build:\n    runs-on: ubuntu-latest\n",
    )
    .unwrap();
    fs::write(
        base.join("Dockerfile"),
        "FROM node:22\nENV TARGET_BETA=1\n",
    )
    .unwrap();

    // Warm index
    let st = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(st.status.success());

    // 1. `dependents` on generic `shared.ts` shows import line + `uses:` and ignores prose "shared" in ci.yml
    let dep = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["dependents", "src/team-settings/shared.ts", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let dep_s = String::from_utf8_lossy(&dep.stdout);
    assert!(dep_s.contains("page.tsx:1  uses: getItem, helperB") || dep_s.contains("page.tsx:1  uses: helperB, getItem"), "got:\n{dep_s}");
    assert!(!dep_s.contains("ci.yml"), "generic stem 'shared' should not match prose in ci.yml, got:\n{dep_s}");
    assert!(dep_s.contains("test/fixture dependent file hidden"), "got:\n{dep_s}");

    // 2. File-scoped `callers shared.ts:getItem --context 1` merges lines 4 & 5 into one hunk & hides test by default
    let cal = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["callers", "shared.ts:getItem", "--context", "1", "--no-noncode", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let cal_s = String::from_utf8_lossy(&cal.stdout);
    assert!(cal_s.contains("page.tsx:"), "got:\n{cal_s}");
    assert!(!cal_s.contains("dashboard.test.ts:"), "test caller should be collapsed by default, got:\n{cal_s}");
    assert_eq!(cal_s.matches("     4│").count(), 1, "merged context should not duplicate line 4:\n{cal_s}");
    assert_eq!(cal_s.matches("     5│").count(), 1, "merged context should not duplicate line 5:\n{cal_s}");

    // 3. `outline` compact default + `--no-comments` inside expanded body + line-range slicing
    let out_compact = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["outline", "src/team-settings/shared.ts", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let oc = String::from_utf8_lossy(&out_compact.stdout);
    assert!(oc.contains("L1-4") && oc.contains("[function]"), "expected compact 1-line-per-symbol outline, got:\n{oc}");

    let out_exp = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["outline", "src/team-settings/shared.ts", "getItem", "--no-comments", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let oe = String::from_utf8_lossy(&out_exp.stdout);
    assert!(!oe.contains("internal comment"), "--no-comments must strip comments in expanded bodies, got:\n{oe}");

    let out_slice = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["outline", "Dockerfile", "1-2", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let os = String::from_utf8_lossy(&out_slice.stdout);
    assert!(os.contains("FROM node:22") && os.contains("TARGET_BETA=1"), "expected line-range slice on Dockerfile, got:\n{os}");

    // 4. `grep` alternation branch counts + `--type dockerfile` + `--path` 0-match warning
    let g_alt = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "DashboardView|NEVER_EXISTS_XYZ", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let ga = String::from_utf8_lossy(&g_alt.stdout);
    assert!(ga.contains("Pattern matches (files): DashboardView: 1 | NEVER_EXISTS_XYZ: 0"), "got:\n{ga}");

    let g_type = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "TARGET_BETA", "--type", "dockerfile", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&g_type.stdout).contains("Dockerfile:"));

    let g_zero = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "getItem", "--path", "/nonexistent/abs/path.ts", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&g_zero.stderr).contains("matched 0 files"));

    // 5. Path-aware `search "Team Settings"` + `betterrank files`
    let srch = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["search", "Team Settings", "--no-noncode", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let ss = String::from_utf8_lossy(&srch.stdout);
    assert!(ss.contains("src/team-settings/page.tsx"), "path-aware search should surface Next.js route file, got:\n{ss}");

    let fls = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["files", "team-settings", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let fs_out = String::from_utf8_lossy(&fls.stdout);
    assert!(fs_out.contains("src/team-settings/page.tsx") && fs_out.contains("src/team-settings/shared.ts"), "got:\n{fs_out}");

    let _ = fs::remove_dir_all(&base);
}
