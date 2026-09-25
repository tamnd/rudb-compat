-- fires
-- A NULL join key joins nothing, so movie_keyword row 8 and row 9 must not survive as if NULL were a value.
SELECT MIN(mk.id) AS first, MAX(mk.id) AS last FROM movie_keyword AS mk, keyword AS k, title AS t
WHERE mk.keyword_id = k.id AND mk.movie_id = t.id;
