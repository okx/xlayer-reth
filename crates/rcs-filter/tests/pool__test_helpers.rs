use super::*;

impl BufferPool {
    #[cfg(test)]
    pub(crate) fn apply_submit_response_current(&mut self, accepted: &[String], now: u64) {
        let expected = accepted
            .iter()
            .filter_map(|hash| parse_hash(hash))
            .filter_map(|hash| self.entries.get(&hash).map(|entry| (hash, entry.generation)))
            .collect();
        self.apply_submit_response(accepted, &expected, &FilterConfig::default(), now);
    }

    #[cfg(test)]
    pub(crate) fn apply_query_status_current(
        &mut self,
        tx_hash: &B256,
        status: &str,
        now: u64,
    ) -> Option<QueryResolution> {
        let generation = self.entries.get(tx_hash)?.generation;
        self.apply_query_status(tx_hash, generation, status, &FilterConfig::default(), now)
    }
}
