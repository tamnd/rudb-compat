-- fires
-- No row survives the filter, so the answer is one row of NULLs and not no row at all.
SELECT MIN(t.title) AS movie_title, MIN(mi.info) AS info FROM title AS t, movie_info AS mi
WHERE t.id = mi.movie_id AND mi.info = 'Atlantis';
