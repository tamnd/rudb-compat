-- fires
-- Rows survive, but the column the MIN reads is NULL on every one of them, which is also a NULL answer.
SELECT MIN(mi.note) AS note, MIN(t.title) AS movie_title FROM title AS t, movie_info AS mi
WHERE t.id = mi.movie_id AND mi.info = 'Japan';
