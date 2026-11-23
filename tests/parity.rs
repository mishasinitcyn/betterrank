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

fn write_file(root: &std::path::Path, rel: &str, content: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
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
    assert!(
        dep_s.contains("Tests (1):") && dep_s.contains("src/__tests__/dashboard.test.ts"),
        "expected compact Tests (1) footer, got:\n{dep_s}"
    );

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

#[test]
fn v051_bugs_and_ergonomics() {
    let base = temp_dir("v051-suite");
    let cache_dir = base.join("cache");
    let repo = base.join("mono");
    let sibling = base.join("sibling-repo");

    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(sibling.join(".git")).unwrap();
    fs::create_dir_all(repo.join("packages/pkg-alpha/src")).unwrap();
    fs::create_dir_all(repo.join("packages/pkg-beta/src")).unwrap();
    fs::create_dir_all(repo.join("apps/web/src/routes")).unwrap();
    fs::create_dir_all(repo.join("apps/api/src/handlers")).unwrap();
    fs::create_dir_all(repo.join("apps/web/locales")).unwrap();
    fs::create_dir_all(repo.join("tests/unit")).unwrap();
    fs::create_dir_all(repo.join(".github/workflows")).unwrap();

    // Workspace packages + unrelated package.json mentioning "src/index.ts"
    fs::write(repo.join("package.json"), r#"{"name":"mono-root","private":true}"#).unwrap();
    fs::write(
        repo.join("packages/pkg-alpha/package.json"),
        r#"{"name":"@demo/pkg-alpha","main":"src/index.ts","exports":{".":"./src/index.ts"}}"#,
    )
    .unwrap();
    fs::write(
        repo.join("packages/pkg-alpha/src/index.ts"),
        "export function alphaCore(x: number) {\n  return x + 1;\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("packages/pkg-beta/package.json"),
        r#"{"name":"@demo/pkg-beta","main":"src/index.ts"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("packages/pkg-beta/src/index.ts"),
        "export const betaVal = 2;\n",
    )
    .unwrap();

    // Consumer importing workspace package `@demo/pkg-alpha`
    fs::write(
        repo.join("apps/web/package.json"),
        r#"{"name":"@demo/web","dependencies":{"@demo/pkg-alpha":"workspace:*"}}"#,
    )
    .unwrap();
    fs::write(
        repo.join("apps/web/src/routes/home.ts"),
        "import { alphaCore } from '@demo/pkg-alpha';\n\nexport function renderHome() {\n  const marker = 'MULTI_PATH_TOKEN';\n  return alphaCore(1) + marker.length;\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("apps/api/src/handlers/status.ts"),
        "import { alphaCore } from '@demo/pkg-alpha';\n\nexport function statusCheck() {\n  const token = 'MULTI_PATH_TOKEN';\n  return alphaCore(2) + token.length;\n}\n",
    )
    .unwrap();
    // Plain-word file `drift.ts` + prose "drift" in workflow
    fs::write(
        repo.join("apps/api/src/handlers/drift.ts"),
        "export function computeDrift() { return 0; }\n",
    )
    .unwrap();
    fs::write(
        repo.join(".github/workflows/nightly.yml"),
        "name: nightly\n# Check for schema drift nightly\njobs:\n  run:\n    steps:\n      - run: echo ok\n",
    )
    .unwrap();
    // Test file + noise data files (*.dic and locales/*.json)
    fs::write(
        repo.join("tests/unit/home.test.ts"),
        "import { alphaCore } from '@demo/pkg-alpha';\nexport function testHome() {\n  return alphaCore(99) + 'TEST_ONLY_TOKEN'.length;\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("apps/web/locales/en.json"),
        r#"{"title": "MULTI_PATH_TOKEN"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("apps/web/words.dic"),
        "MULTI_PATH_TOKEN\n",
    )
    .unwrap();
    // Sibling repo file for Bug #3 test
    fs::write(
        sibling.join("only_in_sibling.ts"),
        "export function siblingFn() { return 42; }\n",
    )
    .unwrap();
    // Long file (180 lines) to verify explicit line-range slicing is NOT chopped at 140 lines
    let mut long_src = String::new();
    for i in 1..=180 {
        long_src.push_str(&format!("export const line_{i} = {i};\n"));
    }
    fs::write(repo.join("apps/api/src/long.ts"), &long_src).unwrap();

    // Warm index for `mono`
    let warm = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(warm.status.success());

    // Bug #1: repeatable `--path` is OR-union (both apps/web and apps/api appear)
    let multi_path = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "MULTI_PATH_TOKEN",
            "--path",
            "apps/web",
            "--path",
            "apps/api",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(multi_path.status.success());
    let mp_s = String::from_utf8_lossy(&multi_path.stdout);
    assert!(
        mp_s.contains("apps/web/src/routes/home.ts") && mp_s.contains("apps/api/src/handlers/status.ts"),
        "repeatable --path must OR-union, got:\n{mp_s}"
    );
    // Upgrade #4: skips locales/*.json and *.dic by default
    assert!(
        !mp_s.contains("locales/en.json") && !mp_s.contains("words.dic"),
        "locales/*.json and *.dic should be skipped by default, got:\n{mp_s}"
    );

    // Upgrade #4: `--exclude-path`, `--no-imports`, and `--compact`
    let excl = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "alphaCore",
            "--no-imports",
            "--exclude-path",
            "apps/api",
            "--compact",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let excl_s = String::from_utf8_lossy(&excl.stdout);
    assert!(excl_s.contains("apps/web/src/routes/home.ts  L5"), "expected home.ts L5 only (--no-imports omits L1):\n{excl_s}");
    assert!(!excl_s.contains("apps/api/src/handlers/status.ts"), "--exclude-path failed:\n{excl_s}");
    assert!(!excl_s.contains("home.ts  L1"), "--no-imports should omit home.ts line 1 import:\n{excl_s}");

    // Bug #4: `--path tests/unit` auto-includes test hits without needing `--include-tests`
    let test_path = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "TEST_ONLY_TOKEN",
            "--path",
            "tests/unit",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let tp_s = String::from_utf8_lossy(&test_path.stdout);
    assert!(
        tp_s.contains("tests/unit/home.test.ts"),
        "--path tests/unit should auto-include tests, got:\n{tp_s}"
    );

    // Bug #3: `outline <abs-path-in-sibling> --annotate` from parent `base` dir must walk up from target file's `.git`
    let sib_out = Command::new(bin_path())
        .current_dir(&base)
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "outline",
            sibling.join("only_in_sibling.ts").to_str().unwrap(),
            "--annotate",
        ])
        .output()
        .unwrap();
    assert!(sib_out.status.success());
    let sib_err = String::from_utf8_lossy(&sib_out.stderr);
    assert!(
        sib_err.contains("Indexed 1 files"),
        "expected outline --annotate to auto-root to sibling .git (1 file), got stderr:\n{sib_err}"
    );

    // Bug #5: `dependents packages/pkg-alpha/src/index.ts` finds code dependents with import lines,
    // mentions `@demo/pkg-alpha` in apps/web/package.json, and does NOT match unrelated pkg-beta/package.json!
    let dep_pkg = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "dependents",
            "packages/pkg-alpha/src/index.ts",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let dp_s = String::from_utf8_lossy(&dep_pkg.stdout);
    assert!(dp_s.contains("apps/web/src/routes/home.ts:1"), "expected import line via @demo/pkg-alpha, got:\n{dp_s}");
    assert!(dp_s.contains("Tests (1):") && dp_s.contains("tests/unit/home.test.ts"), "expected Tests footer, got:\n{dp_s}");
    assert!(!dp_s.contains("pkg-beta/package.json"), "must not match bare index.ts in unrelated package.json:\n{dp_s}");

    // Bug #5b: plain-word `drift.ts` must NOT match prose "drift" in nightly.yml
    let dep_drift = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "dependents",
            "apps/api/src/handlers/drift.ts",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let dd_s = String::from_utf8_lossy(&dep_drift.stdout);
    assert!(!dd_s.contains("nightly.yml"), "plain word 'drift' must not match prose in nightly.yml:\n{dd_s}");

    // Upgrade #1: multi-file batched `outline`, always-on headers, `--filter`, and full explicit line range >140 lines
    let multi_out = Command::new(bin_path())
        .args([
            "outline",
            "apps/web/src/routes/home.ts:1-4",
            "apps/api/src/handlers/status.ts:statusCheck",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(multi_out.status.success());
    let mo_s = String::from_utf8_lossy(&multi_out.stdout);
    assert!(mo_s.contains("── apps/web/src/routes/home.ts:1-4 ──"), "got:\n{mo_s}");
    assert!(mo_s.contains("── statusCheck (apps/api/src/handlers/status.ts:"), "got:\n{mo_s}");

    let long_slice = Command::new(bin_path())
        .args([
            "outline",
            "apps/api/src/long.ts",
            "1-180",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ls_s = String::from_utf8_lossy(&long_slice.stdout);
    assert!(ls_s.contains("180│ export const line_180 = 180;"), "explicit 1-180 range must not be chopped at 140 lines:\n{ls_s}");

    let filt_out = Command::new(bin_path())
        .args([
            "outline",
            "apps/api/src/long.ts",
            "--filter",
            "line_17[89]",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fo_s = String::from_utf8_lossy(&filt_out.stdout);
    assert!(fo_s.contains("line_178") && fo_s.contains("line_179") && !fo_s.contains("line_1 "), "got:\n{fo_s}");

    // Upgrade #2: `files` auto-rollup when > --limit and explicit `--dirs`
    let rollup = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "files",
            "apps",
            "--limit",
            "2",
            "--root",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ru_s = String::from_utf8_lossy(&rollup.stdout);
    assert!(ru_s.contains("apps/api/") && ru_s.contains("apps/web/"), "expected directory rollup, got:\n{ru_s}");

    // Bug #2: Unix SIGPIPE exits cleanly without Rust panic when piped to `head -n 1`
    #[cfg(unix)]
    {
        let cmd_str = format!(
            "{} outline {} 1-180 --root {} | head -n 1",
            bin_path().display(),
            "apps/api/src/long.ts",
            repo.display()
        );
        let piped = Command::new("sh").args(["-c", &cmd_str]).output().unwrap();
        let err_s = String::from_utf8_lossy(&piped.stderr);
        assert!(
            !err_s.contains("panicked") && !err_s.contains("Broken pipe"),
            "SIGPIPE must not panic, got stderr:\n{err_s}"
        );
    }

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn v052_quotas_budget_files_and_outlines() {
    let base = temp_dir("v052-suite");
    let cache_dir = base.join("cache");
    let repo_a = base.join("repo-a");
    let repo_b = base.join("repo-b");

    fs::create_dir_all(repo_a.join("apps/web")).unwrap();
    fs::create_dir_all(repo_a.join("apps/api")).unwrap();
    fs::create_dir_all(repo_a.join("pkg")).unwrap();
    fs::create_dir_all(repo_b.join("src")).unwrap();

    // 1. Huge file (> 20 KB) to test default 16 KB non-TTY cap + exact resume hint + `--max-chars 0`
    let mut huge_ts = String::from("export function bigProcessor() {\n");
    for i in 1..=450 {
        huge_ts.push_str(&format!(
            "  const step_{i:03} = 'padding_payload_value_for_line_{i:03}_testing';\n"
        ));
    }
    huge_ts.push_str("  const needle = 'SPECIAL_AROUND_TARGET';\n");
    huge_ts.push_str("  return needle;\n}\n");
    fs::write(repo_a.join("apps/api/huge.ts"), &huge_ts).unwrap();

    // 2. Same-file alternation starvation + comment demotion + `-w`
    let mut mixed_ts = String::new();
    for i in 1..=25 {
        mixed_ts.push_str(&format!("const noisy_{i} = 'NOISY_TERM';\n"));
    }
    mixed_ts.push_str("const rare = 'RARE_TERM';\n");
    mixed_ts.push_str("const cls = 'boxShadow shadowMode';\n");
    mixed_ts.push_str("const exact = shadow;\n");
    fs::write(repo_a.join("apps/api/mixed.ts"), &mixed_ts).unwrap();

    // Multiple files with NOISY_TERM so NOISY_TERM outnumbers RARE_TERM across files too
    for i in 1..=6 {
        fs::write(
            repo_a.join(format!("apps/web/noisy_{i}.ts")),
            format!("export const n{i} = 'NOISY_TERM';\n"),
        )
        .unwrap();
    }
    fs::write(
        repo_a.join("apps/web/rare_only.ts"),
        "export const onlyRare = 'RARE_TERM';\n",
    )
    .unwrap();

    // Dockerfile with 8 `# comment` lines mentioning `pip install` before real `RUN pip install`
    let mut df = String::from("FROM python:3.11\n");
    for i in 1..=8 {
        df.push_str(&format!("# comment {i}: remember pip install order\n"));
    }
    df.push_str("RUN pip install --no-cache-dir demo-pkg\n");
    fs::write(repo_a.join("worker.Dockerfile"), &df).unwrap();

    // Python __init__.py with __all__ and _LAZY_EXPORTS
    fs::write(
        repo_a.join("pkg/__init__.py"),
        "__all__ = ['alpha', 'beta']\n\n_LAZY_EXPORTS = {\n    'alpha': 'pkg.alpha',\n    'beta': 'pkg.beta',\n}\n",
    )
    .unwrap();

    // Large JSON config (> 60 lines) to test structured key outline
    let mut big_json = String::from("{\n  \"name\": \"demo-app\",\n  \"scripts\": {\n");
    for i in 1..=65 {
        let comma = if i < 65 { "," } else { "" };
        big_json.push_str(&format!("    \"task_{i}\": \"echo {i}\"{comma}\n"));
    }
    big_json.push_str("  },\n  \"dependencies\": {\n    \"foo\": \"1.0.0\"\n  }\n}\n");
    fs::write(repo_a.join("package.json"), &big_json).unwrap();

    // Sibling repo with 0 hits for `RARE_TERM`
    fs::write(repo_b.join("src/hello.ts"), "export const hello = 1;\n").unwrap();

    // Warm index for repo-a
    let warm = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(warm.status.success());

    // A. Default non-TTY `outline` caps at <= 16 KB with exact resume command; `--max-chars 0` uncaps
    let capped = Command::new(bin_path())
        .args(["outline", "apps/api/huge.ts:1-453", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let cap_s = String::from_utf8_lossy(&capped.stdout);
    assert!(cap_s.len() < 17_000, "non-TTY outline must cap near 16KB, got {} bytes", cap_s.len());
    assert!(
        cap_s.contains("next: betterrank outline apps/api/huge.ts:L"),
        "expected exact resume hint, got tail:\n{}",
        &cap_s[cap_s.len().saturating_sub(300)..]
    );

    let uncapped = Command::new(bin_path())
        .args([
            "outline",
            "apps/api/huge.ts:1-453",
            "--max-chars",
            "0",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let uncap_s = String::from_utf8_lossy(&uncapped.stdout);
    assert!(uncap_s.contains("SPECIAL_AROUND_TARGET"), "--max-chars 0 must uncap:\n{}", uncap_s.len());

    // B. `outline file:sym --around <regex> -C 2`, `:ALL`, and structured JSON key outline
    let around = Command::new(bin_path())
        .args([
            "outline",
            "apps/api/huge.ts:bigProcessor",
            "--around",
            "SPECIAL_AROUND_TARGET",
            "-C",
            "2",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ar_s = String::from_utf8_lossy(&around.stdout);
    assert!(ar_s.contains("SPECIAL_AROUND_TARGET") && ar_s.contains("step_450"), "got:\n{ar_s}");
    assert!(!ar_s.contains("step_001"), "--around should only show context window:\n{ar_s}");

    let json_out = Command::new(bin_path())
        .args(["outline", "package.json", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let jo_s = String::from_utf8_lossy(&json_out.stdout);
    assert!(jo_s.contains("dependencies") && jo_s.contains("scripts") && !jo_s.contains("\"echo 30\""), "expected JSON key outline, got:\n{jo_s}");

    // C. Python `__init__.py` includes `__all__` and `_LAZY_EXPORTS`
    let py_out = Command::new(bin_path())
        .args(["outline", "pkg/__init__.py", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let py_s = String::from_utf8_lossy(&py_out.stdout);
    assert!(py_s.contains("__all__") && py_s.contains("_LAZY_EXPORTS"), "expected Python module constants in outline, got:\n{py_s}");

    // D. Rarest-branch-first `grep` quotas (both file-level --limit AND same-file --max-per-file) + multi-root `Repos (files):`
    let g_bal = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "NOISY_TERM|RARE_TERM",
            "--limit",
            "3",
            "--max-per-file",
            "3",
            "--root",
            repo_a.to_str().unwrap(),
            "--root",
            repo_b.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gb_s = String::from_utf8_lossy(&g_bal.stdout);
    assert!(gb_s.contains("Repos (files): repo-a:") && gb_s.contains("repo-b: 0"), "expected per-repo header, got:\n{gb_s}");
    assert!(gb_s.contains("RARE_TERM"), "rare branch must NOT starve behind noisy branch:\n{gb_s}");

    let g_same = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "NOISY_TERM|RARE_TERM",
            "--path",
            "apps/api/mixed.ts",
            "--max-per-file",
            "3",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gs_s = String::from_utf8_lossy(&g_same.stdout);
    assert!(gs_s.contains("const rare = 'RARE_TERM'"), "same-file rare branch must not starve behind 25 noisy lines:\n{gs_s}");

    // E. Comment demotion in Dockerfile + `-w` word boundary
    let g_df = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "pip install",
            "--path",
            "worker.Dockerfile",
            "--max-per-file",
            "3",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gdf_s = String::from_utf8_lossy(&g_df.stdout);
    assert!(gdf_s.contains("RUN pip install"), "comments must be demoted below RUN pip install:\n{gdf_s}");

    let g_word = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "shadow",
            "-w",
            "--path",
            "apps/api/mixed.ts",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gw_s = String::from_utf8_lossy(&g_word.stdout);
    assert!(gw_s.contains("const exact = shadow;") && !gw_s.contains("boxShadow"), "-w must match whole word only:\n{gw_s}");

    // F. All 3 `files` fixes: multiple positional dirs, `--depth 2` doesn't hide tiny dirs, `--filter <regex>`
    let f_multi = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "files",
            "apps/api",
            "pkg",
            "--depth",
            "2",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fm_s = String::from_utf8_lossy(&f_multi.stdout);
    assert!(
        fm_s.contains("apps/api/huge.ts") && fm_s.contains("pkg/__init__.py"),
        "files must accept multiple positional dirs and not roll up small dirs on --depth:\n{fm_s}"
    );

    let f_filt = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "files",
            "apps",
            "--filter",
            "rare_only",
            "--limit",
            "2",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ff_s = String::from_utf8_lossy(&f_filt.stdout);
    assert!(ff_s.contains("apps/web/rare_only.ts") && !ff_s.contains("directories"), "--filter must run before rollup:\n{ff_s}");

    // G. `search --path` filters symbol results
    let s_path = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "search",
            "bigProcessor",
            "--path",
            "apps/web",
            "--no-noncode",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let sp_s = String::from_utf8_lossy(&s_path.stdout);
    assert!(sp_s.contains("(no matches"), "search --path apps/web must exclude apps/api symbol:\n{sp_s}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn v053_fair_outline_schema_files_and_quotas() {
    let base = temp_dir("v053-suite");
    let cache_dir = base.join("cache");
    let repo_a = base.join("repo-a");
    let repo_b = base.join("repo-b");

    fs::create_dir_all(repo_a.join(".git")).unwrap();
    fs::create_dir_all(repo_b.join(".git")).unwrap();
    fs::create_dir_all(repo_a.join("apps/qa/src/app")).unwrap();
    fs::create_dir_all(repo_a.join("apps/qa/src/server")).unwrap();
    fs::create_dir_all(repo_a.join("apps/qa-extra")).unwrap();
    fs::create_dir_all(repo_a.join("packages/schema/prisma")).unwrap();
    fs::create_dir_all(repo_a.join("tests")).unwrap();
    fs::create_dir_all(repo_b.join("src")).unwrap();

    // 1. Huge markdown (25 KB) + TOML with 10 KB single-line comment + small target slice
    let mut huge_md = String::from("# Big Guide\n");
    for i in 1..=320 {
        huge_md.push_str(&format!(
            "Line {i:03}: detailed architectural paragraph padding text for water-filling test.\n"
        ));
    }
    fs::write(repo_a.join("BIG_GUIDE.md"), &huge_md).unwrap();

    let huge_comment = format!("# {}\n[project]\nname = \"demo\"\n", "x".repeat(10_000));
    fs::write(repo_a.join("pyproject.toml"), &huge_comment).unwrap();

    let mut imp_ts = String::new();
    for i in 1..=10 {
        imp_ts.push_str(&format!("import {{ dep{i} }} from './dep{i}';\n"));
    }
    imp_ts.push_str("\nexport function importantSlice() {\n  return 'SLICE_MUST_APPEAR';\n}\n");
    fs::write(repo_a.join("apps/qa/src/server/slice.ts"), &imp_ts).unwrap();

    // 2. Large schema.prisma (~620 KB > old 512 KB cap!)
    let mut big_prisma = String::from("model DemoEntity {\n  id String @id\n}\n");
    for i in 1..=7000 {
        big_prisma.push_str(&format!(
            "model FillerModel{i} {{\n  id String @id\n  label String // padding comment {i:04}\n}}\n"
        ));
    }
    big_prisma.push_str("model TargetSchemaRecord {\n  id String @id\n}\n");
    fs::write(
        repo_a.join("packages/schema/prisma/schema.prisma"),
        &big_prisma,
    )
    .unwrap();

    // 3. Files inside apps/qa/src/app and apps/qa/src/server + apps/qa-extra
    for i in 1..=3 {
        fs::write(
            repo_a.join(format!("apps/qa/src/app/page_{i}.ts")),
            format!("export const p{i} = {i};\n"),
        )
        .unwrap();
        fs::write(
            repo_a.join(format!("apps/qa/src/server/svc_{i}.ts")),
            format!("export const s{i} = {i};\n"),
        )
        .unwrap();
    }
    fs::write(repo_a.join("apps/qa-extra/other.ts"), "export const other = 1;\n").unwrap();

    // 4. Huge router file with 250 private helpers BEFORE `export const mainRouter`
    let mut giant_router = String::new();
    for i in 1..=250 {
        giant_router.push_str(&format!(
            "function privateHelperWithLongDescriptiveName_{i:03}(arg: string): string {{ return arg + '{i}'; }}\n"
        ));
    }
    giant_router.push_str("export const mainRouter = {\n  submitItem: () => 1,\n};\n");
    fs::write(repo_a.join("apps/qa/src/server/giant-router.ts"), &giant_router).unwrap();

    // 5. Receiver-qualified callers fixture (realistic multi-line tRPC routers)
    fs::write(
        repo_a.join("apps/qa/src/server/review.ts"),
        "export const reviewRouter = createTRPCRouter({\n  getEntry: publicProcedure\n    .query(() => 'review'),\n});\n",
    )
    .unwrap();
    fs::write(
        repo_a.join("apps/qa/src/server/audit.ts"),
        "export const auditRouter = createTRPCRouter({\n  getEntry: publicProcedure\n    .query(() => 'audit'),\n});\n",
    )
    .unwrap();
    fs::write(
        repo_a.join("apps/qa/src/app/caller-review.ts"),
        "export function runA(api: any) { return api.review.getEntry.useQuery(); }\n",
    )
    .unwrap();
    fs::write(
        repo_a.join("apps/qa/src/app/caller-audit.ts"),
        "export function runB(api: any) { return api.audit.getEntry.useQuery(); }\n",
    )
    .unwrap();

    // 6. Multi-repo comment monopoly vs sibling code + multi-scope branch header
    for i in 1..=10 {
        fs::write(
            repo_a.join(format!("apps/qa/src/server/comment_only_{i}.ts")),
            format!("// CROSS_REPO_TOKEN mentioned only in comment {i}\nexport const c{i} = {i};\n"),
        )
        .unwrap();
    }
    fs::write(
        repo_a.join("tests/scope_only.test.ts"),
        "export function testScope() { return 'TEST_SCOPED_TERM'; }\n",
    )
    .unwrap();
    fs::write(
        repo_b.join("src/real_code.ts"),
        "export function siblingConsumer() {\n  return 'CROSS_REPO_TOKEN';\n}\n",
    )
    .unwrap();

    // Warm repo-a
    let warm = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(warm.status.success());

    // Fix #1: Fair water-filling + explicit-slice priority + ultra-long line cap + import collapse
    let batch = Command::new(bin_path())
        .args([
            "outline",
            "BIG_GUIDE.md:ALL",
            "pyproject.toml:ALL",
            "apps/qa/src/server/slice.ts:12-14",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let bs = String::from_utf8_lossy(&batch.stdout);
    assert!(bs.contains("SLICE_MUST_APPEAR"), "trailing explicit slice must NEVER be dropped:\n{bs}");
    assert!(bs.contains("chars truncated"), "10KB single line in pyproject.toml should be capped:\n{bs}");
    assert!(bs.contains("name = \"demo\""), "pyproject.toml content after huge comment must still appear:\n{bs}");

    let imp_col = Command::new(bin_path())
        .args(["outline", "apps/qa/src/server/slice.ts:ALL", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let ics = String::from_utf8_lossy(&imp_col.stdout);
    assert!(ics.contains("[imports collapsed: L1-10"), "expected collapsed imports in :ALL, got:\n{ics}");
    assert!(ics.contains("SLICE_MUST_APPEAR"), "got:\n{ics}");

    // Fix #2: 620 KB schema.prisma appears in `files`, `grep --code`, AND `grep --non-code`!
    let f_prisma = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["files", "--filter", r"schema\.prisma", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&f_prisma.stdout).contains("packages/schema/prisma/schema.prisma"),
        "620KB schema.prisma must appear in `files`, got:\n{}",
        String::from_utf8_lossy(&f_prisma.stdout)
    );
    for flag in ["--code", "--non-code"] {
        let g_pr = Command::new(bin_path())
            .env("CODE_INDEX_CACHE_DIR", &cache_dir)
            .args([
                "grep",
                "TargetSchemaRecord",
                flag,
                "--root",
                repo_a.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let gs = String::from_utf8_lossy(&g_pr.stdout);
        assert!(
            gs.contains("packages/schema/prisma/schema.prisma"),
            "schema.prisma must be searchable under grep {flag}, got:\n{gs}"
        );
    }

    // Fix #3a: `files apps/qa --limit 4` rolls up into `apps/qa/src/app/` and `apps/qa/src/server/`, NEVER `apps/`!
    let f_qa = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["files", "apps/qa", "--limit", "4", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let fqs = String::from_utf8_lossy(&f_qa.stdout);
    assert!(
        fqs.contains("apps/qa/src/app/") && fqs.contains("apps/qa/src/server/"),
        "files apps/qa must roll up to children (not parent apps/), got:\n{fqs}"
    );
    assert!(!fqs.contains("qa-extra"), "files apps/qa must not leak apps/qa-extra:\n{fqs}");

    // Fix #3b: `files /abs/path/to/repo-b` from parent `base` dir auto-roots to `repo-b/.git`!
    let f_abs = Command::new(bin_path())
        .current_dir(&base)
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["files", repo_b.to_str().unwrap(), "--filter", "real_code"])
        .output()
        .unwrap();
    let fas = String::from_utf8_lossy(&f_abs.stdout);
    let fae = String::from_utf8_lossy(&f_abs.stderr);
    assert!(fas.contains("src/real_code.ts"), "positional repo dir in files should work:\n{fas}\nstderr:\n{fae}");
    assert!(fae.contains("Indexed 1 files"), "must index ONLY repo-b (1 file), not parent:\n{fae}");

    // Fix #4a: Export-first `outline` on giant router surfaces `mainRouter` despite 250 private helpers before it!
    let exp_out = Command::new(bin_path())
        .args([
            "outline",
            "apps/qa/src/server/giant-router.ts",
            "--max-chars",
            "4000",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let eos = String::from_utf8_lossy(&exp_out.stdout);
    assert!(eos.contains("mainRouter"), "export-first outline must surface exported mainRouter:\n{eos}");

    // Fix #4b: Multi-root `grep` reserves per-repo slots so repo-b appears even with 10 comment files in repo-a!
    let g_multi = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "CROSS_REPO_TOKEN",
            "--limit",
            "4",
            "--root",
            repo_a.to_str().unwrap(),
            "--root",
            repo_b.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gms = String::from_utf8_lossy(&g_multi.stdout);
    assert!(gms.contains("repo-b/src/real_code.ts"), "multi-root grep must include sibling repo-b:\n{gms}");

    // Fix #4c: `--inventory` includes enclosing symbol names & multi-scope branch split shows `code/non-code/tests`
    let g_inv = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "SLICE_MUST_APPEAR",
            "--inventory",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gis = String::from_utf8_lossy(&g_inv.stdout);
    assert!(
        gis.contains("apps/qa/src/server/slice.ts (1): importantSlice"),
        "expected inventory format with enclosing symbol, got:\n{gis}"
    );

    let g_scopes = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "NEVER_EXISTS_ZZZ|TEST_SCOPED_TERM",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gsc = String::from_utf8_lossy(&g_scopes.stdout);
    assert!(
        gsc.contains("NEVER_EXISTS_ZZZ: 0/0/0") && gsc.contains("TEST_SCOPED_TERM: 0/0/1"),
        "expected code/non-code/tests branch split, got:\n{gsc}"
    );

    // Fix #4d: Receiver-qualified `callers review.getEntry` excludes `audit.getEntry`!
    let c_recv = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "callers",
            "review.getEntry",
            "--no-noncode",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let crs = String::from_utf8_lossy(&c_recv.stdout);
    assert!(crs.contains("caller-review.ts") && !crs.contains("caller-audit.ts"), "receiver-qualified callers failed:\n{crs}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn v054_universal_fixes_and_grep_outline_upgrades() {
    let base = temp_dir("v054-suite");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("apps/web/src/app/(dashboard)/item/[id]")).unwrap();
    fs::create_dir_all(base.join("packages/core/src")).unwrap();
    fs::create_dir_all(base.join("tests/fixtures")).unwrap();
    fs::create_dir_all(base.join(".github/workflows")).unwrap();

    // 1. Next.js route-group path with `(dashboard)` and `[id]` (> 120 lines)
    let mut route_src = String::from("export function ItemPage() {\n");
    for i in 1..=120 {
        route_src.push_str(&format!(
            "  const row_{i:03} = 'route_payload_value_{i:03}_extra_padding';\n"
        ));
    }
    route_src.push_str("  return row_001;\n}\n");
    fs::write(
        base.join("apps/web/src/app/(dashboard)/item/[id]/page.tsx"),
        &route_src,
    )
    .unwrap();

    // 2. Import-redirect & self-recursive caller & space-separated callers
    fs::write(
        base.join("packages/core/src/helper.ts"),
        "export function helperAlpha(n: number): number {\n  if (n <= 0) return 0;\n  return helperAlpha(n - 1) + 1;\n}\nexport function helperBeta() { return 2; }\n",
    )
    .unwrap();
    fs::write(
        base.join("apps/web/src/consumer.ts"),
        "import { helperAlpha, helperBeta } from '../../../packages/core/src/helper';\nexport function runBoth() {\n  const x = 'CO_OCCUR_A';\n  const y = 'CO_OCCUR_B';\n  return helperAlpha(2) + helperBeta() + x.length + y.length;\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("packages/core/src/only_a.ts"),
        "export const onlyA = 'CO_OCCUR_A';\n",
    )
    .unwrap();

    // 3. Prisma schema with models & enums
    fs::write(
        base.join("schema.prisma"),
        "model DemoOrder {\n  id String @id\n  status String\n}\n\nenum OrderState {\n  OPEN\n  CLOSED\n}\n",
    )
    .unwrap();

    // 4. Python file with triple-quoted docstring mentioning token vs real code
    fs::write(
        base.join("packages/core/src/mod.py"),
        "def compute():\n    \"\"\"\n    DOCSTRING_ONLY_TOKEN inside triple quotes\n    \"\"\"\n    # hash comment\n    return 'REAL_PY_CODE'\n",
    )
    .unwrap();

    // 5. Test-only term + `.txt` + `.jsonl.gz` fixture files + Dockerfile + CI yml
    fs::write(
        base.join("tests/only_in_tests.test.ts"),
        "export function testOnly() { return 'TEST_ONLY_AUTOPROMOTE_TOKEN'; }\n",
    )
    .unwrap();
    fs::write(base.join("tests/fixtures/notes.txt"), "sample notes\n").unwrap();
    fs::write(base.join("tests/fixtures/samples.jsonl.gz"), b"fake-gz-bytes").unwrap();
    fs::write(base.join("worker.Dockerfile"), "FROM python:3.11\nENV GLOB_TOKEN=1\n").unwrap();
    fs::write(
        base.join(".github/workflows/ci.yml"),
        "name: ci\nenv:\n  GLOB_TOKEN: 1\n",
    )
    .unwrap();

    // Warm index
    let warm = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(warm.status.success());

    // A. Paren-path `(dashboard)` resume hint must NOT strip `(dashboard)` and MUST shell-quote!
    let paren = Command::new(bin_path())
        .args([
            "outline",
            "apps/web/src/app/(dashboard)/item/[id]/page.tsx:1-120",
            "--max-chars",
            "800",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ps = String::from_utf8_lossy(&paren.stdout);
    assert!(
        ps.contains("next: betterrank outline 'apps/web/src/app/(dashboard)/item/[id]/page.tsx:L"),
        "expected intact quoted route-group path in resume hint, got:\n{ps}"
    );

    // B. Explicit `:ALL` overrides `--full-under 50`, while bare file respects `--full-under 50`!
    let fu_all = Command::new(bin_path())
        .args([
            "outline",
            "apps/web/src/app/(dashboard)/item/[id]/page.tsx:ALL",
            "--full-under",
            "50",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fus_all = String::from_utf8_lossy(&fu_all.stdout);
    assert!(
        fus_all.contains("row_050"),
        "explicit :ALL must override --full-under 50, got:\n{fus_all}"
    );
    let fu_bare = Command::new(bin_path())
        .args([
            "outline",
            "apps/web/src/app/(dashboard)/item/[id]/page.tsx",
            "--full-under",
            "50",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fus_bare = String::from_utf8_lossy(&fu_bare.stdout);
    assert!(
        fus_bare.contains("[function]") && !fus_bare.contains("row_050"),
        "bare file with --full-under 50 on 122-line file should return compact skeleton, got:\n{fus_bare}"
    );

    // C. `.prisma` block slicing + `--around` asymmetric `-B 0 -A 2` + import-redirect hint
    let pr = Command::new(bin_path())
        .args(["outline", "schema.prisma:DemoOrder", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let prs = String::from_utf8_lossy(&pr.stdout);
    assert!(prs.contains("model DemoOrder") && !prs.contains("enum OrderState"), "got:\n{prs}");

    let redir = Command::new(bin_path())
        .args(["outline", "apps/web/src/consumer.ts:helperAlpha", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let rds = String::from_utf8_lossy(&redir.stdout);
    assert!(
        rds.contains("imported at L1") && rds.contains("packages/core/src/helper"),
        "expected import-redirect hint, got:\n{rds}"
    );

    let asym = Command::new(bin_path())
        .args([
            "outline",
            "apps/web/src/app/(dashboard)/item/[id]/page.tsx",
            "--around",
            "row_010",
            "-B",
            "0",
            "-A",
            "2",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let asys = String::from_utf8_lossy(&asym.stdout);
    assert!(asys.contains("row_010") && asys.contains("row_012") && !asys.contains("row_009"), "got:\n{asys}");

    // D. `outline file.py:ALL --no-comments` strips Python docstring + `#` comments
    let py_nc = Command::new(bin_path())
        .args(["outline", "packages/core/src/mod.py:ALL", "--no-comments", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let pncs = String::from_utf8_lossy(&py_nc.stdout);
    assert!(pncs.contains("REAL_PY_CODE") && !pncs.contains("DOCSTRING_ONLY_TOKEN") && !pncs.contains("hash comment"), "got:\n{pncs}");

    // E. Space-separated `callers helperAlpha helperBeta` + self-recursive call in `callers helperAlpha --context 1`
    let c_space = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "callers",
            "helperAlpha",
            "helperBeta",
            "--no-noncode",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let css = String::from_utf8_lossy(&c_space.stdout);
    assert!(css.contains("=== callers: helperAlpha ===") && css.contains("=== callers: helperBeta ==="), "got:\n{css}");

    let c_rec = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "callers",
            "helperAlpha",
            "--context",
            "1",
            "--no-noncode",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let cres = String::from_utf8_lossy(&c_rec.stdout);
    assert!(cres.contains("return helperAlpha(n - 1)"), "self-recursive call must appear in callers --context:\n{cres}");

    // F. `grep`: auto-include tests when 0 prod hits, Python docstring stripping under `--no-comments`,
    // repeatable `--glob`, `--and`, and `--group-by-package`!
    let g_auto = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "TEST_ONLY_AUTOPROMOTE_TOKEN", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let gas = String::from_utf8_lossy(&g_auto.stdout);
    assert!(
        gas.contains("showing test/fixture matches automatically") && gas.contains("only_in_tests.test.ts"),
        "expected auto-promoted test matches when prod=0, got:\n{gas}"
    );

    let g_pydoc = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["grep", "DOCSTRING_ONLY_TOKEN", "--no-comments", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let gpds = String::from_utf8_lossy(&g_pydoc.stdout);
    assert!(gpds.contains("(no matches"), "--no-comments must strip Python triple-quoted docstrings:\n{gpds}");

    let g_glob = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "GLOB_TOKEN",
            "--glob",
            "*Dockerfile*",
            "--glob",
            "*.yml",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ggs = String::from_utf8_lossy(&g_glob.stdout);
    assert!(ggs.contains("worker.Dockerfile") && ggs.contains(".github/workflows/ci.yml"), "got:\n{ggs}");

    let g_and = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "CO_OCCUR_A",
            "--and",
            "CO_OCCUR_B",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gands = String::from_utf8_lossy(&g_and.stdout);
    assert!(gands.contains("apps/web/src/consumer.ts") && !gands.contains("only_a.ts"), "--and co-occurrence failed:\n{gands}");

    let g_pkg = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "CO_OCCUR_A",
            "--group-by-package",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gpkgs = String::from_utf8_lossy(&g_pkg.stdout);
    assert!(gpkgs.contains("apps/web (1 file") && gpkgs.contains("packages/core (1 file"), "--group-by-package failed:\n{gpkgs}");

    // G. `files` includes `.txt` and `.jsonl.gz` fixture files!
    let f_fix = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["files", "tests/fixtures", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    let ffs = String::from_utf8_lossy(&f_fix.stdout);
    assert!(ffs.contains("notes.txt") && ffs.contains("samples.jsonl.gz"), "files should list .txt and .jsonl.gz:\n{ffs}");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn v055_bugfixes_around_ranges_path_boundaries_and_inventory_branches() {
    let base = temp_dir("v055-suite");
    let cache_dir = base.join("cache");
    fs::create_dir_all(base.join("apps/qa/src")).unwrap();
    fs::create_dir_all(base.join("apps/qa-triage/src")).unwrap();

    let mut big = String::new();
    for i in 1..=120 {
        if i == 75 {
            big.push_str("export const target_75 = 'AROUND_RANGE_NEEDLE';\n");
        } else {
            big.push_str(&format!(
                "export const line_{i:03} = 'filler_text_value_for_line_{i:03}_padding';\n"
            ));
        }
    }
    fs::write(base.join("apps/qa/src/big.ts"), &big).unwrap();
    fs::write(
        base.join("apps/qa/src/alpha.ts"),
        "export function runAlpha() { return 'BRANCH_ALPHA'; }\n",
    )
    .unwrap();
    fs::write(
        base.join("apps/qa-triage/src/triage.ts"),
        "export function runTriage() { return 'BRANCH_ALPHA'; }\n",
    )
    .unwrap();

    // Warm index
    let warm = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["stats", "--root", base.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(warm.status.success());

    // 1a. `outline file:L50-100 --around AROUND_RANGE_NEEDLE -C 1` filters inside L50-100 (does NOT dump L50..L73)
    let ar = Command::new(bin_path())
        .args([
            "outline",
            "apps/qa/src/big.ts:L50-100",
            "--around",
            "AROUND_RANGE_NEEDLE",
            "-C",
            "1",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ars = String::from_utf8_lossy(&ar.stdout);
    assert!(
        ars.contains("AROUND_RANGE_NEEDLE") && !ars.contains("line_055"),
        "outline file:L50-100 --around must filter inside range, got:\n{ars}"
    );

    // 1b. Multi-range truncation preserves trailing ranges in `next:` AND omits `--max-chars 0` bait
    let mr = Command::new(bin_path())
        .args([
            "outline",
            "apps/qa/src/big.ts:L1-40,L50-80,L100-115",
            "--max-chars",
            "700",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let mrs = String::from_utf8_lossy(&mr.stdout);
    assert!(
        mrs.contains("L50-80") && mrs.contains("L100-115") && !mrs.contains("--max-chars 0"),
        "multi-range next: must preserve trailing ranges without --max-chars 0 bait, got:\n{mrs}"
    );

    // 2a. `grep --path apps/qa` must NOT leak `apps/qa-triage/src/triage.ts`
    let gp = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "BRANCH_ALPHA",
            "--path",
            "apps/qa",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gps = String::from_utf8_lossy(&gp.stdout);
    assert!(
        gps.contains("apps/qa/src/alpha.ts") && !gps.contains("qa-triage"),
        "grep --path apps/qa must not leak apps/qa-triage, got:\n{gps}"
    );

    // 2b. `files /abs/root/apps/qa --root /abs/root` lists files cleanly
    let fabs = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "files",
            base.join("apps/qa").to_str().unwrap(),
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fabss = String::from_utf8_lossy(&fabs.stdout);
    assert!(
        fabss.contains("apps/qa/src/alpha.ts") && !fabss.contains("qa-triage"),
        "files /abs/sub --root /abs/root must work, got:\n{fabss}"
    );

    // 3. `grep -l` / `--inventory` includes matched alternation branch tags + enclosing symbol
    let ginv = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "BRANCH_ALPHA|AROUND_RANGE_NEEDLE",
            "--inventory",
            "--path",
            "apps/qa",
            "--root",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let ginvs = String::from_utf8_lossy(&ginv.stdout);
    assert!(
        ginvs.contains("apps/qa/src/alpha.ts (1) [BRANCH_ALPHA]: runAlpha"),
        "inventory should include branch tag + enclosing symbol, got:\n{ginvs}"
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn v056_batching_scoping_json_imports_and_callers_fixes() {
    let base = temp_dir("v056-fixes");
    let cache_dir = base.join(".cache");
    let repo_a = base.join("repos/repo-a");
    let repo_b = base.join("repos/repo-b");
    fs::create_dir_all(repo_a.join(".git")).unwrap();
    fs::create_dir_all(repo_b.join(".git")).unwrap();

    // 1. JSON & YAML array-of-objects configs (>40 lines)
    let mut cron_json = String::from("{\n  \"crons\": [\n");
    for i in 0..15 {
        let comma = if i + 1 < 15 { "," } else { "" };
        cron_json.push_str(&format!(
            "    {{\n      \"path\": \"/api/cron/job-{i}\",\n      \"schedule\": \"0 {i} * * 1\"\n    }}{comma}\n"
        ));
    }
    cron_json.push_str("  ]\n}\n");
    write_file(&repo_a, "config/schedule.json", &cron_json);

    let mut rules_yaml = String::from("rules:\n");
    for i in 0..16 {
        rules_yaml.push_str(&format!(
            "  - path: /rules/item-{i}\n    role: role_{i}\n    mode: strict\n"
        ));
    }
    write_file(&repo_a, "config/rules.yaml", &rules_yaml);

    // 2. File with >=6 import lines + local imports used in body (for collapsed-imports footer)
    write_file(&repo_a, "src/helper.ts", "export function computeTotal(x: number) { return x * 2; }\n");
    write_file(&repo_a, "src/unused.ts", "export function unusedHelper() { return 0; }\n");
    write_file(
        &repo_a,
        "src/feature.ts",
        r#"import React from 'react';
import { z } from 'zod';
import { computeTotal } from './helper';
import { unusedHelper } from './unused';
import { sharedUtil } from '@acme/core';
import type { Foo } from '@types/node';

export function runFeature(n: number) {
  return computeTotal(n) + sharedUtil(n);
}
"#,
    );

    // 3. Multi-target `outline fileA --around "needle" fileB:targetFn`
    let mut file_a = String::new();
    for i in 1..=60 {
        if i == 30 {
            file_a.push_str("const hit = 'AROUND_BATCH_NEEDLE';\n");
        } else {
            file_a.push_str(&format!("const pad_{i} = {i};\n"));
        }
    }
    write_file(&repo_a, "src/scan.ts", &file_a);
    write_file(
        &repo_a,
        "src/target.ts",
        "export function targetFn() {\n  return 'TARGET_FN_BODY_OK';\n}\nexport function otherFn() {\n  return 1;\n}\n",
    );

    // 4. Multi-file `dependents` + `callers` line numbers + `--path` auto-root from non-git parent
    write_file(&repo_a, "apps/svc/lib-a.ts", "export function sharedA() { return 1; }\n");
    write_file(&repo_a, "apps/svc/lib-b.ts", "export function sharedB() { return 2; }\n");
    write_file(
        &repo_a,
        "apps/svc/consumer.ts",
        "import { sharedA } from './lib-a';\nimport { sharedB } from './lib-b';\nexport function runBoth() {\n  return sharedA() + sharedB();\n}\n",
    );
    write_file(&repo_b, "src/other.ts", "export function unrelated() { return 99; }\n");

    // 5. Multi-dir `files` rollup without collapsing back up to `apps/`
    for sub in ["alpha", "beta", "gamma"] {
        for i in 1..=3 {
            write_file(
                &repo_a,
                &format!("apps/{sub}/mod/f{i}.ts"),
                &format!("export const {sub}_{i} = {i};\n"),
            );
        }
    }

    // 6. Single-file `grep` with 35 matches (should not cap at 5/25/30!)
    let mut dense = String::from("export function denseFn() {\n");
    for i in 1..=35 {
        dense.push_str(&format!("  const v{i} = 'DENSE_SINGLE_FILE_TOKEN_{i}';\n"));
    }
    dense.push_str("  return v1;\n}\n");
    write_file(&repo_a, "src/dense.ts", &dense);

    // 7. `grep --all-scopes` with Python docstring-only files + Dockerfile + CI YAML
    for i in 1..=12 {
        write_file(
            &repo_a,
            &format!("py/doc_{i}.py"),
            &format!(
                "def fn_{i}():\n    \"\"\"\n    Mentions DEPLOY_MARKER_XYZ only in a docstring.\n    \"\"\"\n    return {i}\n"
            ),
        );
    }
    write_file(&repo_a, "Dockerfile", "FROM python:3.11\nRUN echo DEPLOY_MARKER_XYZ\n");
    write_file(
        &repo_a,
        ".github/workflows/ci.yml",
        "name: ci\njobs:\n  build:\n    steps:\n      - run: echo DEPLOY_MARKER_XYZ\n",
    );

    // Verify 1: JSON & YAML array-of-objects compact previews (not bare repeated keys!)
    let jo = Command::new(bin_path())
        .args(["outline", "config/schedule.json", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let jos = String::from_utf8_lossy(&jo.stdout);
    assert!(
        jos.contains("[0] path=\"/api/cron/job-0\"") && jos.contains("schedule=\"0 0 * * 1\""),
        "JSON array-of-objects outline should show compact key=val previews, got:\n{jos}"
    );
    let yo = Command::new(bin_path())
        .args(["outline", "config/rules.yaml", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let yos = String::from_utf8_lossy(&yo.stdout);
    assert!(
        yos.contains("- path: /rules/item-0, role: role_0"),
        "YAML list-of-objects outline should show compact key: val previews, got:\n{yos}"
    );

    // Verify 2: Collapsed imports footer lists used local imports, skips external & unused
    let imp_o = Command::new(bin_path())
        .args(["outline", "src/feature.ts:ALL", "--root", repo_a.to_str().unwrap()])
        .output()
        .unwrap();
    let imp_s = String::from_utf8_lossy(&imp_o.stdout);
    assert!(
        imp_s.contains("local imports used:")
            && imp_s.contains("./helper (computeTotal)")
            && imp_s.contains("@acme/core (sharedUtil)")
            && !imp_s.contains("unusedHelper")
            && !imp_s.contains("'react'"),
        "collapsed imports should show used local imports only, got:\n{imp_s}"
    );

    // Verify 3: Mixed multi-target `outline fileA --around "re" fileB:sym` does NOT zero out fileB:sym
    let mix = Command::new(bin_path())
        .args([
            "outline",
            "src/scan.ts",
            "src/target.ts:targetFn",
            "--around",
            "AROUND_BATCH_NEEDLE",
            "-C",
            "2",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let mixs = String::from_utf8_lossy(&mix.stdout);
    assert!(
        mixs.contains("AROUND_BATCH_NEEDLE") && mixs.contains("TARGET_FN_BODY_OK"),
        "mixed multi-target --around must keep explicit sibling symbol slice, got:\n{mixs}"
    );

    // Verify 4a: Multi-file `dependents fileA fileB` handles BOTH files
    let deps = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "dependents",
            "apps/svc/lib-a.ts",
            "apps/svc/lib-b.ts",
            "--no-noncode",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let depss = String::from_utf8_lossy(&deps.stdout);
    assert!(
        depss.contains("=== dependents: apps/svc/lib-a.ts ===")
            && depss.contains("=== dependents: apps/svc/lib-b.ts ===")
            && depss.contains("apps/svc/consumer.ts"),
        "dependents should support multiple files, got:\n{depss}"
    );

    // Verify 4b: `callers` without `--context` includes line numbers AND auto-roots from non-git parent via `--path`
    let call_auto = Command::new(bin_path())
        .current_dir(&base)
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args(["callers", "sharedA", "--path", "apps/svc", "--no-noncode"])
        .output()
        .unwrap();
    let call_s = String::from_utf8_lossy(&call_auto.stdout);
    let call_e = String::from_utf8_lossy(&call_auto.stderr);
    assert!(
        call_s.contains("apps/svc/consumer.ts") && call_s.contains("L4"),
        "callers without --context should print line numbers and auto-scope from non-git parent:\nstdout:\n{call_s}\nstderr:\n{call_e}"
    );
    assert!(
        call_e.contains("Auto-detected --root from --path"),
        "expected auto-root note on stderr, got:\n{call_e}"
    );

    // Verify 4c: `files apps/alpha apps/beta apps/gamma --limit 4` rolls up per dir, NEVER parent `apps/`
    let fmulti = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "files",
            "apps/alpha",
            "apps/beta",
            "apps/gamma",
            "--limit",
            "4",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let fms = String::from_utf8_lossy(&fmulti.stdout);
    assert!(
        fms.contains("apps/alpha/mod/")
            && fms.contains("apps/beta/mod/")
            && fms.contains("apps/gamma/mod/"),
        "multi-dir files rollup must stay under each requested dir, got:\n{fms}"
    );

    // Verify 5a: Single-file `grep --path src/dense.ts` shows all 35 matches (no `(+N more in file)`)
    let gdense = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "DENSE_SINGLE_FILE_TOKEN",
            "--path",
            "src/dense.ts",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gds = String::from_utf8_lossy(&gdense.stdout);
    assert!(
        gds.contains("DENSE_SINGLE_FILE_TOKEN_35") && !gds.contains("more in file"),
        "single-file grep --path must not cap at 5/25/30 matches, got:\n{gds}"
    );

    // Verify 5b: `grep --all-scopes --limit 5` surfaces Dockerfile & ci.yml ahead of Python docstrings
    let gas = Command::new(bin_path())
        .env("CODE_INDEX_CACHE_DIR", &cache_dir)
        .args([
            "grep",
            "DEPLOY_MARKER_XYZ",
            "--all-scopes",
            "--limit",
            "5",
            "--root",
            repo_a.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let gass = String::from_utf8_lossy(&gas.stdout);
    assert!(
        gass.contains("Dockerfile") && gass.contains(".github/workflows/ci.yml"),
        "grep --all-scopes must surface Dockerfile and CI YAML instead of drowning in Python docstrings, got:\n{gass}"
    );

    let _ = fs::remove_dir_all(&base);
}
