UPDATE permissions
SET description = replace(description, ' (legacy placeholder; service-first runtime does not implement contest business in this rebase)', '')
WHERE code LIKE 'contest.%';

UPDATE roles
SET description = replace(description, ' (legacy placeholder; not enabled as current business capability)', '')
WHERE name LIKE 'contest_%';
