const REPO_TERMS: Record<string, string> = {
  Bundle: "Repo",
  Bundles: "Repos",
  bundle: "repo",
  bundles: "repos",
};

/** Translate the internal protocol term without rewriting IDs or object paths. */
export function userFacingRepoTerms(value: string): string {
  return value.replace(
    /(^|[\s([{"'“])(Bundles|Bundle|bundles|bundle)(?=$|[\s)\]},.!?:;"'”])/g,
    (_match, prefix: string, term: string) => `${prefix}${REPO_TERMS[term]}`,
  );
}
