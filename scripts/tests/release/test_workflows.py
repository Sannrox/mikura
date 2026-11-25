"""Release invariants no workflow linter expresses, asserted on the parsed YAML.

The scripts enforce these at run time; these tests keep the workflows from
bypassing them. Assert on structure (triggers, permissions, job graph, step
order, which script a step runs), never on the wording of a step.
"""

import unittest

import yaml

from scripts.tests.release import ROOT

WORKFLOWS = ROOT / ".github" / "workflows"
VERIFIED_SHA = "${{ needs.verify-tag.outputs.sha }}"
TAGGED = "needs.verify-tag.outputs.tag != ''"


def load(name: str) -> dict:
    doc = yaml.safe_load((WORKFLOWS / name).read_text())
    doc["on"] = doc.pop(True, doc.get("on"))  # YAML 1.1 reads a bare `on` key as True.
    return doc


def runs(step: dict) -> str:
    return step.get("run", "")


def uses(step: dict) -> str:
    return step.get("uses", "")


def is_mutation(step: dict) -> bool:
    return (
        uses(step).startswith("actions/attest")
        or "github-release.sh draft" in runs(step)
        or "github-release.sh publish" in runs(step)
    )


def is_guard(step: dict) -> bool:
    return "github-release.sh guard" in runs(step)


def needs(job: dict) -> set:
    value = job.get("needs", [])
    return {value} if isinstance(value, str) else set(value)


def strings(node):
    if isinstance(node, dict):
        for value in node.values():
            yield from strings(value)
    elif isinstance(node, list):
        for value in node:
            yield from strings(value)
    elif isinstance(node, str):
        yield node


class Dispatcher(unittest.TestCase):
    """A v* tag push only starts release.yml from main; it can do nothing else."""

    def setUp(self):
        self.wf = load("release-dispatch.yml")

    def test_triggers_only_on_v_tags(self):
        self.assertEqual(self.wf["on"], {"push": {"tags": ["v*"]}})

    def test_can_only_dispatch_workflows(self):
        self.assertEqual(self.wf["permissions"], {"actions": "write", "contents": "read"})

    def test_runs_one_step_that_dispatches_release_on_main(self):
        [job] = self.wf["jobs"].values()
        self.assertNotIn("permissions", job)
        [step] = job["steps"]
        self.assertNotIn("uses", step)
        self.assertEqual(step["run"].strip(), 'gh workflow run release.yml --ref main -f tag="$TAG"')


class Release(unittest.TestCase):
    def setUp(self):
        self.wf = load("release.yml")
        self.jobs = self.wf["jobs"]

    def test_never_runs_on_push(self):
        self.assertNotIn("push", self.wf["on"])
        self.assertIn("tag", self.wf["on"]["workflow_dispatch"]["inputs"])

    def test_one_run_per_tag(self):
        self.assertIn("inputs.tag", self.wf["concurrency"]["group"])
        self.assertFalse(self.wf["concurrency"]["cancel-in-progress"])

    def test_refuses_a_tagged_run_not_started_from_main_before_anything_else(self):
        first = self.jobs["verify-tag"]["steps"][0]
        self.assertEqual(first.get("if"), "inputs.tag != ''")
        self.assertNotIn("uses", first)
        self.assertEqual(first["env"]["WORKFLOW_REF"], "${{ github.ref }}")
        self.assertIn("refs/heads/main", first["run"])

    def test_verifies_the_tag_with_mains_verifier_and_requires_it_on_main(self):
        steps = self.jobs["verify-tag"]["steps"]
        main_checkout = next(s for s in steps if s.get("with", {}).get("path") == "main-tree")
        self.assertNotIn("ref", main_checkout["with"], "main's verifier must come from the workflow ref")
        verify = next(s for s in steps if "verify-tag.sh" in runs(s))
        self.assertIn("${MAIN_TREE}/scripts/release/verify-tag.sh", verify["run"])
        self.assertTrue(verify["env"]["MAIN_TREE"].endswith("/main-tree"))
        self.assertIn("${MAIN_TREE}/security/release-tag-fingerprints", verify["run"])
        self.assertIn("merge-base --is-ancestor", verify["run"])
        resolve = next(i for i, s in enumerate(steps) if s.get("id") == "resolve")
        self.assertLess(steps.index(verify), resolve)

    def test_every_later_job_builds_the_verified_commit(self):
        for name, job in self.jobs.items():
            if name == "verify-tag":
                continue
            with self.subTest(job=name):
                self.assertIn("verify-tag", needs(job))
                steps = job["steps"]
                checkouts = [i for i, s in enumerate(steps) if uses(s).startswith("actions/checkout@")]
                self.assertTrue(checkouts, f"{name} must check out the release commit")
                for i in checkouts:
                    self.assertEqual(steps[i]["with"].get("ref"), VERIFIED_SHA)
                    self.assertIn('test "$(git rev-parse HEAD)" = "$SHA"', runs(steps[i + 1]))
                    self.assertEqual(steps[i + 1]["env"]["SHA"], VERIFIED_SHA)

    def test_never_uses_the_event_commit_or_ref_name(self):
        for value in strings(self.jobs):
            for banned in ("github.sha", "github.ref_name", "GITHUB_SHA", "GITHUB_REF", "github.event_name"):
                self.assertNotIn(banned, value)

    def test_release_changes_go_through_the_release_script(self):
        for value in strings(self.jobs):
            self.assertNotIn("gh release ", value)

    def test_every_mutating_job_runs_only_for_a_tag_and_refuses_a_published_release_first(self):
        mutating = set()
        for name, job in self.jobs.items():
            steps = job["steps"]
            first = next((i for i, s in enumerate(steps) if is_mutation(s)), None)
            if first is None:
                continue
            mutating.add(name)
            with self.subTest(job=name):
                guards = [s for s in steps[:first] if is_guard(s)]
                self.assertTrue(guards, f"{name} mutates before the guard")
                self.assertTrue(all("if" not in s for s in guards), f"{name} guard must always run")
                self.assertEqual(job.get("if"), TAGGED)
        self.assertEqual(mutating, {"attest", "publish"})
        verify_guard = [s for s in self.jobs["verify-tag"]["steps"] if is_guard(s)]
        self.assertEqual(len(verify_guard), 1, "a tagged run refuses a published release before it builds")

    def test_publish_runs_last_and_only_after_every_asset_job_succeeded(self):
        publish = self.jobs["publish"]
        self.assertLessEqual({"package", "attest"}, needs(publish))
        self.assertIn("package", needs(self.jobs["attest"]))
        self.assertFalse(any("publish" in needs(job) for job in self.jobs.values()))
        # A status function in `if` would replace the implicit success() and
        # let a job run after a failed dependency.
        for name, job in self.jobs.items():
            for fn in ("always()", "failure()", "cancelled()"):
                with self.subTest(job=name, fn=fn):
                    self.assertNotIn(fn, str(job.get("if", "")))

    def test_takeover_drill_gates_packaging(self):
        drill = self.jobs["drill"]
        self.assertTrue(any("--bench takeover" in runs(s) for s in drill["steps"]))
        self.assertIn("drill", needs(self.jobs["package"]))
        self.assertNotIn("if", drill, "the drill runs for every release and dry run")

    def test_publish_records_attestations_then_drafts_notes_from_changelog(self):
        steps = self.jobs["publish"]["steps"]
        record = next(i for i, s in enumerate(steps) if "manifest.py record-attestations" in runs(s))
        notes = next(i for i, s in enumerate(steps) if "scripts/release/notes.py" in runs(s))
        draft = next(i for i, s in enumerate(steps) if "github-release.sh draft" in runs(s))
        publish = next(i for i, s in enumerate(steps) if "github-release.sh publish" in runs(s))
        self.assertLess(max(record, notes), draft)
        self.assertLess(draft, publish)

    def test_write_permissions_are_job_scoped(self):
        self.assertEqual(self.wf["permissions"], {"contents": "read"})
        writers = {
            name: {k for k, v in job.get("permissions", {}).items() if v == "write"}
            for name, job in self.jobs.items()
        }
        self.assertEqual(
            {name: perms for name, perms in writers.items() if perms},
            {"attest": {"id-token", "attestations"}, "publish": {"contents"}},
        )


if __name__ == "__main__":
    unittest.main()
