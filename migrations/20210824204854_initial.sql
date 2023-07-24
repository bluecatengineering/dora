-- Add migration script here
-- would prefer to use an enum where the entry is either leased or
-- on probation. expires_at for lease = true refers to when lease expires,
-- if probabtion = true it is when the probation expires
CREATE TABLE IF NOT EXISTS leases(
    ip INTEGER NOT NULL,
    client_id BLOB,
    leased BOOLEAN NOT NULL DEFAULT 0,
    expires_at INTEGER NOT NULL,
    network INTEGER NOT NULL,
    probation BOOLEAN NOT NULL DEFAULT 0,
    PRIMARY KEY(ip)
);
CREATE INDEX idx_ip_expires on leases (ip, expires_at);