# Insert inside /etc/ufw/after.init's start) branch (with set -e).
# Run after UFW's ordinary rules are restored, never in before.init.
# IPBAN is required: report failure without removing ordinary UFW protection.
/usr/sbin/nft --file /etc/ufw/bds-ipban.nft

# Observation is optional; neither its load nor diagnostic failure is fatal.
if ! /usr/sbin/nft --file /etc/ufw/bds-ip-history.nft; then
    bds_ip_history_error='IP history could not be loaded; observation rules were not replaced. Continuing UFW startup.'
    printf '%s\n' "$bds_ip_history_error" >&2 || :
    /usr/bin/logger --tag bds-ip-history --priority daemon.err -- "$bds_ip_history_error" || :
    unset bds_ip_history_error
fi
