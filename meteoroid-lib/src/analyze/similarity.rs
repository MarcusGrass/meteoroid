pub(super) fn similarity(a: &str, b: &str) -> bool {
    // Seems to get pretty good results on normalized levenshtein
    let similarity = strsim::normalized_levenshtein(a, b);
    similarity > 0.9
}
