UPDATE permissions
SET description = description || ' (legacy placeholder; service-first runtime does not implement contest business in this rebase)'
WHERE code LIKE 'contest.%'
  AND description NOT LIKE '%legacy placeholder%';

UPDATE roles
SET description = COALESCE(description, '') || ' (legacy placeholder; not enabled as current business capability)'
WHERE name LIKE 'contest_%'
  AND COALESCE(description, '') NOT LIKE '%legacy placeholder%';
