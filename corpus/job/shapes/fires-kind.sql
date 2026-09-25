-- fires
-- A type table on a leaf, a filter with OR inside one relation, and a NULL kind_id on title 7.
SELECT MIN(t.title) AS movie_title, MAX(kt.kind) AS kind FROM title AS t, kind_type AS kt, movie_companies AS mc
WHERE t.kind_id = kt.id AND t.id = mc.movie_id AND (kt.kind = 'movie' OR kt.kind = 'video game') AND mc.note IS NULL;
