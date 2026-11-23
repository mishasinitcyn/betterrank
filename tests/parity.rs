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
