build:
	@cargo build --workspace --all-targets

test:
	@cargo nextest run --all-features

test-cargo:
	@cargo test --workspace --all-targets

fmt:
	@cargo +nightly fmt --all -- --check

clippy:
	@cargo clippy --workspace --all-targets -- -D warnings

clippy-pedantic:
	@cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic

doc:
	@RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

audit:
	@cargo audit

deny:
	@cargo deny check

frontend-install:
	@if [ -f apps/server/web/package-lock.json ]; then \
		npm ci --prefix apps/server/web; \
	elif [ -f apps/server/web/package.json ]; then \
		npm install --prefix apps/server/web; \
	else \
		echo "frontend assets are not present yet"; \
	fi

frontend-build:
	@if [ -f apps/server/web/package.json ]; then \
		npm run build --prefix apps/server/web; \
	else \
		echo "frontend assets are not present yet"; \
	fi

frontend-typecheck:
	@if [ -f apps/server/web/package.json ]; then \
		npm run typecheck --prefix apps/server/web; \
	else \
		echo "frontend assets are not present yet"; \
	fi

frontend-test:
	@if [ -f apps/server/web/package.json ]; then \
		npm test --prefix apps/server/web; \
	else \
		echo "frontend assets are not present yet"; \
	fi

ci: build test-cargo fmt clippy clippy-pedantic doc audit deny frontend-typecheck frontend-build frontend-test

check-agent-sync:
	@cmp -s CLAUDE.md AGENTS.md || { \
		echo "AGENTS.md must stay in sync with CLAUDE.md"; \
		echo "Update both files with the same shared project instructions."; \
		exit 1; \
	}
	@tmp_dir=$$(mktemp -d); \
	trap 'rm -rf "$$tmp_dir"' EXIT; \
	cp -R .claude/skills "$$tmp_dir/expected-skills"; \
	find "$$tmp_dir/expected-skills" -name SKILL.md -exec perl -0pi -e 's/CLAUDE\.md/AGENTS.md/g; s/Claude/Codex/g; s/claude/codex/g' {} +; \
	diff -ru --exclude agents "$$tmp_dir/expected-skills" .agents/skills || { \
		echo "Codex skills must stay in sync with Claude skills after Claude-to-Codex renaming."; \
		echo "Update .claude/skills first, then mirror the shared content into .agents/skills."; \
		exit 1; \
	}

release:
	@cargo release tag --execute
	@git cliff -o CHANGELOG.md
	@git commit -a -n -m "Update CHANGELOG.md" || true
	@git push origin master
	@cargo release push --execute

update-submodule:
	@git submodule update --init --recursive --remote

.PHONY: build test test-cargo fmt clippy clippy-pedantic doc audit deny frontend-install frontend-build frontend-typecheck frontend-test ci check-agent-sync release update-submodule
